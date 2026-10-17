//! Drives an fx agent over the Agent Client Protocol.
//!
//! The UI talks to [`AgentHandle`] through plain channels and never sees the
//! transport. Today the transport is an `fx acp` child process; the in-process
//! libfx build will speak the same ACP bytes, so only [`Launch`] changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ClientCapabilities, ClientSessionCapabilities, ContentBlock, ImageContent, InitializeRequest,
    ListSessionsRequest, NewSessionRequest, NoticeCapabilities, PermissionOption, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionId, SessionInfo, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, StopReason, SubagentCapabilities, TextContent,
    ToolCallUpdate,
};
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, Agent, Client, ConnectTo, ConnectionTo, LineDirection, UntypedMessage,
};
use async_channel::{Receiver, Sender};

pub mod replay;

pub use agent_client_protocol::schema::v1 as acp;

/// How to reach the agent.
pub enum Launch {
    /// Spawn `fx acp` and speak ACP over its stdio.
    Process {
        program: PathBuf,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// Append every ACP line to this JSONL file, for replay fixtures.
        record: Option<PathBuf>,
        /// Gives every session fx's `show_view` tool.
        views: Option<Views>,
    },
    /// Play a recorded session back instead of running fx.
    Replay {
        fixture: PathBuf,
        pace: replay::Pace,
        /// The model the recording's sessions started with.
        model: Option<String>,
        /// Answers the recording's `show_view` calls, as with fx.
        views: Option<Views>,
    },
}

/// The client's side of fx's `show_view` tool: the view format the model is
/// told about, and the check each call's view goes through. fx owns the
/// tool; the format is the client's, so it is sent at initialize as
/// `clientCapabilities._meta.sense.views`.
#[derive(Clone)]
pub struct Views {
    pub description: String,
    /// JSON Schema of the tool's arguments.
    pub input_schema: serde_json::Value,
    /// Says what was shown, or what is wrong with the view: either way, the
    /// text becomes the tool result the model reads. Runs on the agent's
    /// connection, so it must not block.
    pub check: CheckView,
}

/// See [`Views::check`].
pub type CheckView = Arc<dyn Fn(&serde_json::Value) -> Result<String, String> + Send + Sync>;

/// fx runs each `show_view` call by asking the client.
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, agent_client_protocol::JsonRpcRequest,
)]
#[request(method = "_sense/show_view", response = ShowViewResponse)]
#[serde(rename_all = "camelCase")]
struct ShowViewRequest {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    input: serde_json::Value,
}

#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, agent_client_protocol::JsonRpcResponse,
)]
#[serde(rename_all = "camelCase")]
struct ShowViewResponse {
    content: String,
    is_error: bool,
}

pub enum Command {
    /// `tag` comes back on [`Event::SessionCreated`] so the caller can match
    /// the session to whatever asked for it.
    NewSession {
        cwd: PathBuf,
        tag: u64,
    },
    /// Prompts a session. One this agent process hasn't seen (created by an
    /// earlier process, e.g. before a restart or sign-in) is resumed first.
    Prompt {
        session: SessionId,
        cwd: PathBuf,
        text: String,
        /// (mime type, bytes) of each attached image.
        images: Vec<(String, std::sync::Arc<[u8]>)>,
        /// Switches the session to this model first, if it's on another.
        model: Option<String>,
    },
    Cancel {
        session: SessionId,
    },
    /// One page of past sessions, newest first.
    ListSessions {
        cursor: Option<String>,
    },
    /// One page of a saved session's transcript, newest turn first, read
    /// without making it the active session (the fork's
    /// `_sense/session_history`).
    History {
        session: SessionId,
        cursor: Option<String>,
        limit: u32,
    },
}

pub enum Event {
    Ready {
        agent_name: Option<String>,
    },
    SessionCreated {
        tag: u64,
        session: SessionId,
    },
    Update {
        session: SessionId,
        update: SessionUpdate,
    },
    Permission(PermissionAsk),
    SessionFailed {
        tag: u64,
        error: String,
    },
    /// `error` is set when the turn ended because something failed.
    TurnEnded {
        session: SessionId,
        stop_reason: Option<StopReason>,
        error: Option<String>,
    },
    /// Newest turn first; each turn's updates in their own order.
    History {
        session: SessionId,
        turns: Vec<Vec<SessionUpdate>>,
        older: Option<String>,
    },
    HistoryFailed {
        session: SessionId,
        error: String,
    },
    SessionsListed {
        sessions: Vec<SessionInfo>,
        next_cursor: Option<String>,
    },
    /// A failure not tied to a session, e.g. the agent couldn't start.
    Error(String),
    Exited,
}

/// A tool call waiting for the user. Dropping it without answering cancels it.
pub struct PermissionAsk {
    pub session: SessionId,
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
    reply: Sender<Option<usize>>,
}

impl PermissionAsk {
    /// Choose `options[index]`, or `None` to reject the request.
    pub fn answer(self, index: Option<usize>) {
        let _ = self.reply.try_send(index);
    }
}

#[derive(Clone)]
pub struct AgentHandle {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
}

impl AgentHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }

    /// Stops the agent for every holder of this handle, not just this one:
    /// fx exits and [`Event::Exited`] follows.
    pub fn shutdown(&self) {
        self.commands.close();
    }
}

/// Starts the agent on its own thread. Returns immediately; [`Event::Ready`]
/// arrives once the ACP handshake completes.
pub fn spawn(launch: Launch) -> AgentHandle {
    let (commands_tx, commands_rx) = async_channel::unbounded();
    let (events_tx, events_rx) = async_channel::unbounded();
    thread::Builder::new()
        .name("sense-agent".into())
        .spawn(move || {
            let result = smol::block_on(async {
                match launch {
                    Launch::Process {
                        program,
                        args,
                        env,
                        record,
                        views,
                    } => {
                        // New sessions start on the model fx was launched with.
                        let model = env.get("FX_MODEL").cloned();
                        let config = AcpAgentConfig::new(program).args(args).envs(env);
                        let mut agent = AcpAgent::new(config);
                        if let Some(path) = record {
                            agent = agent.with_debug(recorder(&path)?);
                        }
                        serve(agent, commands_rx, events_tx.clone(), views, model).await
                    }
                    Launch::Replay {
                        fixture,
                        pace,
                        model,
                        views,
                    } => {
                        let fixture = replay::Fixture::load(&fixture).map_err(|err| {
                            agent_client_protocol::Error::internal_error().data(err.to_string())
                        })?;
                        let (to_agent_rx, to_agent_tx) = piper::pipe(1 << 16);
                        let (to_client_rx, to_client_tx) = piper::pipe(1 << 16);
                        let replay_events = events_tx.clone();
                        smol::spawn(async move {
                            if let Err(err) = fixture.serve(to_agent_rx, to_client_tx, pace).await {
                                let _ =
                                    replay_events.try_send(Event::Error(format!("replay: {err}")));
                            }
                        })
                        .detach();
                        let transport =
                            agent_client_protocol::ByteStreams::new(to_agent_tx, to_client_rx);
                        serve(transport, commands_rx, events_tx.clone(), views, model).await
                    }
                }
            });
            if let Err(err) = result {
                let _ = events_tx.try_send(Event::Error(err.to_string()));
            }
            let _ = events_tx.try_send(Event::Exited);
        })
        .expect("spawn agent thread");
    AgentHandle {
        commands: commands_tx,
        events: events_rx,
    }
}

/// Writes `{"ms":…,"dir":"send"|"recv"|"stderr","line":…}` per ACP line. The
/// raw line is kept as a string so replay reproduces it byte for byte.
fn recorder(
    path: &std::path::Path,
) -> agent_client_protocol::Result<impl Fn(&str, LineDirection) + Send + Sync + 'static> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| agent_client_protocol::Error::internal_error().data(err.to_string()))?;
    let file = std::sync::Mutex::new(std::io::LineWriter::new(file));
    let start = std::time::Instant::now();
    Ok(move |line: &str, direction: LineDirection| {
        let dir = match direction {
            LineDirection::Stdin => "send",
            LineDirection::Stdout => "recv",
            LineDirection::Stderr => "stderr",
        };
        let record = serde_json::json!({
            "ms": start.elapsed().as_millis() as u64,
            "dir": dir,
            "line": line,
        });
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{record}");
        }
    })
}

async fn serve(
    transport: impl ConnectTo<Client> + 'static,
    commands: Receiver<Command>,
    events: Sender<Event>,
    views: Option<Views>,
    launch_model: Option<String>,
) -> agent_client_protocol::Result<()> {
    let notify_events = events.clone();
    let permission_events = events.clone();
    let check_view = views.as_ref().map(|views| views.check.clone());
    let mut meta = acp::Meta::new();
    if let Some(views) = &views {
        meta.insert(
            "sense".into(),
            serde_json::json!({"views": {
                "description": views.description,
                "inputSchema": views.input_schema,
            }}),
        );
    }
    Client
        .builder()
        .name("sense")
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                let _ = notify_events.try_send(Event::Update {
                    session: notification.session_id,
                    update: notification.update,
                });
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, cx| {
                let (reply_tx, reply_rx) = async_channel::bounded(1);
                let options = request.options.clone();
                let _ = permission_events.try_send(Event::Permission(PermissionAsk {
                    session: request.session_id,
                    tool_call: request.tool_call,
                    options: request.options,
                    reply: reply_tx,
                }));
                // Waiting here would stall the dispatch loop, so the answer is
                // awaited on a connection task.
                cx.spawn(async move {
                    let outcome = match reply_rx.recv().await.ok().flatten() {
                        Some(index) if index < options.len() => RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(options[index].option_id.clone()),
                        ),
                        _ => RequestPermissionOutcome::Cancelled,
                    };
                    responder.respond(RequestPermissionResponse::new(outcome))
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ShowViewRequest, responder, _cx| {
                // The app draws views from the tool call itself, live and
                // in history; this only says whether there is one to draw.
                let checked = match &check_view {
                    Some(check) => check(&request.input),
                    None => Err("this client draws no views".into()),
                };
                responder.respond(match checked {
                    Ok(content) => ShowViewResponse {
                        content,
                        is_error: false,
                    },
                    Err(content) => ShowViewResponse {
                        content,
                        is_error: true,
                    },
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                        // Lets fx send diagnostics as notices instead of
                        // mixing them into the assistant's text.
                        ClientCapabilities::new()
                            .session(
                                ClientSessionCapabilities::new().notices(NoticeCapabilities::new()),
                            )
                            // Subagents stream their own updates, announced
                            // on the parent session first.
                            .subagents(SubagentCapabilities::new())
                            // fx's show_view tool, in the client's format.
                            .meta((!meta.is_empty()).then_some(meta)),
                    ),
                )
                .block_task()
                .await?;
            let _ = events.try_send(Event::Ready {
                agent_name: init.agent_info.map(|info| info.name),
            });

            // Sessions this process created or resumed; others need resuming.
            let known = Arc::new(Mutex::new(HashSet::<SessionId>::new()));
            // Each session's model, where known: what it started with, or
            // was last switched to. Resumed sessions keep whatever fx saved.
            let models = Arc::new(Mutex::new(HashMap::<SessionId, String>::new()));
            while let Ok(command) = commands.recv().await {
                let connection_for_task = connection.clone();
                let events = events.clone();
                match command {
                    Command::NewSession { cwd, tag } => {
                        let known = known.clone();
                        let models = models.clone();
                        let launch_model = launch_model.clone();
                        connection.spawn(async move {
                            let request = NewSessionRequest::new(cwd);
                            match connection_for_task.send_request(request).block_task().await {
                                Ok(response) => {
                                    known
                                        .lock()
                                        .unwrap_or_else(|err| err.into_inner())
                                        .insert(response.session_id.clone());
                                    if let Some(model) = launch_model {
                                        models
                                            .lock()
                                            .unwrap_or_else(|err| err.into_inner())
                                            .insert(response.session_id.clone(), model);
                                    }
                                    let _ = events.try_send(Event::SessionCreated {
                                        tag,
                                        session: response.session_id,
                                    });
                                }
                                Err(err) => {
                                    let _ = events.try_send(Event::SessionFailed {
                                        tag,
                                        error: err.to_string(),
                                    });
                                }
                            }
                            Ok(())
                        })?
                    }
                    Command::Prompt {
                        session,
                        cwd,
                        text,
                        images,
                        model,
                    } => {
                        let known = known.clone();
                        let models = models.clone();
                        connection.spawn(async move {
                            let unseen = known
                                .lock()
                                .unwrap_or_else(|err| err.into_inner())
                                .insert(session.clone());
                            if unseen {
                                let resume = ResumeSessionRequest::new(session.clone(), cwd);
                                if let Err(err) =
                                    connection_for_task.send_request(resume).block_task().await
                                {
                                    known
                                        .lock()
                                        .unwrap_or_else(|err| err.into_inner())
                                        .remove(&session);
                                    let _ = events.try_send(Event::TurnEnded {
                                        session,
                                        stop_reason: None,
                                        error: Some(format!("couldn't resume the session: {err}")),
                                    });
                                    return Ok(());
                                }
                            }
                            let switch = model.filter(|model| {
                                models
                                    .lock()
                                    .unwrap_or_else(|err| err.into_inner())
                                    .get(&session)
                                    != Some(model)
                            });
                            if let Some(model) = switch {
                                let request = SetSessionConfigOptionRequest::new(
                                    session.clone(),
                                    "model",
                                    model.as_str(),
                                );
                                if let Err(err) =
                                    connection_for_task.send_request(request).block_task().await
                                {
                                    let _ = events.try_send(Event::TurnEnded {
                                        session,
                                        stop_reason: None,
                                        error: Some(format!("couldn't switch to {model}: {err}")),
                                    });
                                    return Ok(());
                                }
                                models
                                    .lock()
                                    .unwrap_or_else(|err| err.into_inner())
                                    .insert(session.clone(), model);
                            }
                            // No empty text block for a prompt of only
                            // images: providers reject them.
                            let mut content: Vec<ContentBlock> = (!text.is_empty())
                                .then(|| ContentBlock::Text(TextContent::new(text)))
                                .into_iter()
                                .collect();
                            content.extend(images.iter().map(|(mime, bytes)| {
                                use base64::Engine as _;
                                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                                ContentBlock::Image(ImageContent::new(data, mime.clone()))
                            }));
                            let request = PromptRequest::new(session.clone(), content);
                            let (stop_reason, error) = match connection_for_task
                                .send_request(request)
                                .block_task()
                                .await
                            {
                                Ok(response) => (Some(response.stop_reason), None),
                                Err(err) => (None, Some(err.to_string())),
                            };
                            let _ = events.try_send(Event::TurnEnded {
                                session,
                                stop_reason,
                                error,
                            });
                            Ok(())
                        })?
                    }
                    Command::ListSessions { cursor } => connection.spawn(async move {
                        let request = ListSessionsRequest::new().cursor(cursor);
                        match connection_for_task.send_request(request).block_task().await {
                            Ok(response) => {
                                let _ = events.try_send(Event::SessionsListed {
                                    sessions: response.sessions,
                                    next_cursor: response.next_cursor,
                                });
                            }
                            Err(err) => {
                                let _ = events.try_send(Event::Error(format!(
                                    "couldn't list sessions: {err}"
                                )));
                            }
                        }
                        Ok(())
                    })?,
                    Command::History {
                        session,
                        cursor,
                        limit,
                    } => connection.spawn(async move {
                        let params = serde_json::json!({
                            "sessionId": session.0.as_ref(),
                            "cursor": cursor,
                            "limit": limit,
                        });
                        let request = UntypedMessage::new("_sense/session_history", params)?;
                        let event =
                            match connection_for_task.send_request(request).block_task().await {
                                Ok(response) => match parse_history(&response) {
                                    Ok((turns, older)) => Event::History {
                                        session,
                                        turns,
                                        older,
                                    },
                                    Err(error) => Event::HistoryFailed { session, error },
                                },
                                Err(err) => Event::HistoryFailed {
                                    session,
                                    error: err.to_string(),
                                },
                            };
                        let _ = events.try_send(event);
                        Ok(())
                    })?,
                    Command::Cancel { session } => connection.send_notification(
                        agent_client_protocol::schema::v1::CancelNotification::new(session),
                    )?,
                }
            }
            Ok(())
        })
        .await
}

/// `{turns: [{updates: [...]}], nextCursor?}` into typed updates. An update
/// this client doesn't know is skipped, as it would be live.
fn parse_history(
    response: &serde_json::Value,
) -> Result<(Vec<Vec<SessionUpdate>>, Option<String>), String> {
    let turns = response["turns"]
        .as_array()
        .ok_or("history without turns")?;
    let turns = turns
        .iter()
        .map(|turn| {
            turn["updates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|update| serde_json::from_value(update.clone()).ok())
                .collect()
        })
        .collect();
    let older = response["nextCursor"].as_str().map(str::to_string);
    Ok((turns, older))
}
