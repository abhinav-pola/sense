//! The app's lifecycle as a pure state machine: chats, their fx sessions and
//! turns, and the agent processes behind them.
//!
//! [`Lifecycle::step`] takes one [`Input`] (something the user did, or an
//! event from an agent) and returns the [`Effect`]s to carry out. It does no
//! IO and holds no handles, so every interleaving of inputs can be tested
//! directly; the UI only feeds inputs in and executes the effects.
//!
//! Each chat has its own fx process: fx keeps one active session per process
//! and runs every prompt in it, so chats sharing a process would talk over
//! each other. Agents are numbered, every agent event carries the number of
//! the agent that sent it, and an event counts only if that agent still
//! belongs to its chat. That is what makes sign-out, sign-in, crashes and
//! deletes safe to interleave in any order.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub type ChatId = u64;
pub type AgentId = u64;
pub type SessionId = String;

/// What the user sends: text, any images, and the model to answer with
/// (none: whichever the session has). Opaque to the lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub images: Vec<Image>,
    pub model: Option<String>,
}

impl Prompt {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
            model: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// e.g. `image/png`.
    pub mime: String,
    pub bytes: std::sync::Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    NewChat {
        chat: ChatId,
        cwd: PathBuf,
    },
    /// A past session from fx's store, shown as a chat. Its agent starts,
    /// and resumes the session, only when the chat is used.
    RestoreChat {
        chat: ChatId,
        cwd: PathBuf,
        session: SessionId,
    },
    /// The user is looking at this chat: get its agent ready.
    Focus {
        chat: ChatId,
    },
    /// Only before the chat has a session: fx binds a session to its folder.
    SetFolder {
        chat: ChatId,
        cwd: PathBuf,
    },
    Submit {
        chat: ChatId,
        prompt: Prompt,
    },
    Stop {
        chat: ChatId,
    },
    DeleteChat {
        chat: ChatId,
    },
    /// A credential is available (sign-in, or found at startup).
    SignedIn {
        credential: String,
    },
    SignedOut,
    SessionCreated {
        agent: AgentId,
        chat: ChatId,
        session: SessionId,
    },
    SessionFailed {
        agent: AgentId,
        chat: ChatId,
        error: String,
    },
    TurnEnded {
        agent: AgentId,
        session: SessionId,
        error: Option<String>,
    },
    /// A tool finished in this session; files on disk may have changed.
    ToolFinished {
        agent: AgentId,
        session: SessionId,
    },
    AgentExited {
        agent: AgentId,
    },
    BaselineTaken {
        chat: ChatId,
        error: Option<String>,
    },
    /// The tools agents start with changed (the MCP servers configured, or
    /// one signed in to). fx reads them when a session starts or resumes, so
    /// every agent running now is out of date: each is stopped once its chat
    /// is idle, and the next prompt resumes the session in a fresh one.
    ToolsChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    NewSession {
        chat: ChatId,
        cwd: PathBuf,
    },
    /// The agent resumes a session it hasn't seen before prompting it.
    Prompt {
        session: SessionId,
        cwd: PathBuf,
        prompt: Prompt,
    },
    Cancel {
        session: SessionId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Start an fx process for this chat.
    StartAgent {
        agent: AgentId,
        chat: ChatId,
        credential: String,
    },
    /// Stop this agent; anything it sends afterwards is ignored.
    StopAgent {
        agent: AgentId,
    },
    Send {
        agent: AgentId,
        command: Command,
    },
    /// Snapshot the chat's folder before the agent can change it.
    TakeBaseline {
        chat: ChatId,
        cwd: PathBuf,
    },
    /// Recompute the chat's diff against its baseline.
    RefreshDiff {
        chat: ChatId,
    },
    /// Delete what a removed chat left on disk.
    ForgetChat {
        chat: ChatId,
        session: Option<SessionId>,
    },
    /// Tell the user something about this chat.
    Notify {
        chat: ChatId,
        message: Notice,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    AgentStopped,
    SignedOut,
    /// A prompt is waiting for the user to sign in.
    NeedsSignIn,
    SessionFailed(String),
    TurnFailed(String),
    BaselineFailed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Turn {
    Idle,
    /// Submitted, not yet with the agent: it waits for an agent and a session.
    Waiting {
        prompt: Prompt,
    },
    /// The agent is working on the prompt.
    Running,
    /// Stop was pressed while running and the agent was asked to cancel.
    Cancelling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    NotTaken,
    Taking,
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub cwd: PathBuf,
    pub session: Option<SessionId>,
    pub turn: Turn,
    pub baseline: Baseline,
    /// This chat's fx process, if one is running.
    pub agent: Option<AgentId>,
    /// A `NewSession` was sent to the chat's agent and not yet answered.
    creating: bool,
}

impl Chat {
    pub fn is_busy(&self) -> bool {
        self.turn != Turn::Idle
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    chats: BTreeMap<ChatId, Chat>,
    credential: Option<String>,
    /// Known to be signed out, as opposed to not knowing yet: at startup the
    /// stored credential loads after the first frame, and a prompt typed
    /// meanwhile just waits for it.
    signed_out: bool,
    next_agent: AgentId,
    /// Agents numbered up to this started before the tools last changed.
    stale_through: AgentId,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn chat(&self, chat: ChatId) -> Option<&Chat> {
        self.chats.get(&chat)
    }

    pub fn chats(&self) -> impl Iterator<Item = (ChatId, &Chat)> {
        self.chats.iter().map(|(id, chat)| (*id, chat))
    }

    pub fn signed_in(&self) -> bool {
        self.credential.is_some()
    }

    /// The chat an agent belongs to, if it still belongs to one.
    pub fn chat_of_agent(&self, agent: AgentId) -> Option<ChatId> {
        self.chats
            .iter()
            .find(|(_, chat)| chat.agent == Some(agent))
            .map(|(id, _)| *id)
    }

    /// Which chat a session belongs to.
    pub fn chat_for(&self, session: &str) -> Option<ChatId> {
        self.chats
            .iter()
            .find(|(_, chat)| chat.session.as_deref() == Some(session))
            .map(|(id, _)| *id)
    }

    pub fn step(&mut self, input: Input) -> Vec<Effect> {
        let effects = self.apply(input);
        debug_assert_eq!(self.check_invariants(), Ok(()));
        effects
    }

    fn apply(&mut self, input: Input) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.is_current(&input) {
            return effects;
        }
        match input {
            Input::NewChat { chat: id, cwd } => {
                if self.chats.contains_key(&id) {
                    return effects;
                }
                self.chats.insert(
                    id,
                    Chat {
                        cwd,
                        session: None,
                        turn: Turn::Idle,
                        baseline: Baseline::NotTaken,
                        agent: None,
                        creating: false,
                    },
                );
            }
            Input::RestoreChat {
                chat: id,
                cwd,
                session,
            } => {
                if self.chats.contains_key(&id) || self.chat_for(&session).is_some() {
                    return effects;
                }
                self.chats.insert(
                    id,
                    Chat {
                        cwd,
                        session: Some(session),
                        turn: Turn::Idle,
                        baseline: Baseline::NotTaken,
                        agent: None,
                        creating: false,
                    },
                );
            }
            Input::Focus { chat } => self.ensure_agent(chat, &mut effects),
            Input::SetFolder { chat, cwd } => {
                if let Some(chat) = self.chats.get_mut(&chat)
                    && chat.session.is_none()
                    && !chat.creating
                    && chat.baseline == Baseline::NotTaken
                {
                    chat.cwd = cwd;
                }
            }
            Input::Submit { chat: id, prompt } => {
                let Some(chat) = self.chats.get_mut(&id) else {
                    return effects;
                };
                if chat.is_busy() {
                    return effects;
                }
                if chat.baseline == Baseline::NotTaken {
                    chat.baseline = Baseline::Taking;
                    effects.push(Effect::TakeBaseline {
                        chat: id,
                        cwd: chat.cwd.clone(),
                    });
                }
                chat.turn = Turn::Waiting { prompt };
                if self.signed_out {
                    effects.push(Effect::Notify {
                        chat: id,
                        message: Notice::NeedsSignIn,
                    });
                }
                self.ensure_agent(id, &mut effects);
                self.advance(id, &mut effects);
            }
            Input::Stop { chat: id } => {
                let Some(chat) = self.chats.get_mut(&id) else {
                    return effects;
                };
                match (&chat.turn, chat.agent, &chat.session) {
                    (Turn::Running, Some(agent), Some(session)) => {
                        effects.push(Effect::Send {
                            agent,
                            command: Command::Cancel {
                                session: session.clone(),
                            },
                        });
                        chat.turn = Turn::Cancelling;
                    }
                    // A second Stop while fx hasn't confirmed the cancel: kill
                    // the agent, so the turn can't keep running behind an
                    // idle chat or overlap the next prompt. The next prompt
                    // resumes the session in a fresh process.
                    (Turn::Cancelling, Some(agent), _) => {
                        effects.push(Effect::StopAgent { agent });
                        chat.agent = None;
                        chat.creating = false;
                        chat.turn = Turn::Idle;
                    }
                    // Not yet with the agent: nothing to cancel there.
                    _ => chat.turn = Turn::Idle,
                }
                self.retire_if_stale(id, &mut effects);
            }
            Input::DeleteChat { chat: id } => {
                let Some(chat) = self.chats.remove(&id) else {
                    return effects;
                };
                if let Some(agent) = chat.agent {
                    effects.push(Effect::StopAgent { agent });
                }
                effects.push(Effect::ForgetChat {
                    chat: id,
                    session: chat.session,
                });
            }
            Input::SignedIn { credential } => {
                // A new credential replaces every running agent; they start
                // again, with it, when their chats next need them.
                self.stop_agents(Notice::AgentStopped, &mut effects);
                self.credential = Some(credential);
                self.signed_out = false;
                let waiting: Vec<_> = self
                    .chats
                    .iter()
                    .filter(|(_, chat)| matches!(chat.turn, Turn::Waiting { .. }))
                    .map(|(id, _)| *id)
                    .collect();
                for id in waiting {
                    self.ensure_agent(id, &mut effects);
                    self.advance(id, &mut effects);
                }
            }
            Input::SignedOut => {
                self.stop_agents(Notice::SignedOut, &mut effects);
                self.credential = None;
                self.signed_out = true;
                for (id, chat) in &self.chats {
                    if matches!(chat.turn, Turn::Waiting { .. }) {
                        effects.push(Effect::Notify {
                            chat: *id,
                            message: Notice::NeedsSignIn,
                        });
                    }
                }
            }
            Input::AgentExited { agent } => {
                let id = self.chat_of_agent(agent).expect("checked by is_current");
                let chat = self.chats.get_mut(&id).expect("chat_of_agent found it");
                chat.agent = None;
                chat.creating = false;
                // A prompt that hadn't reached the agent isn't retried on a
                // new one: an agent that dies on start would loop forever.
                if chat.is_busy() {
                    chat.turn = Turn::Idle;
                    effects.push(Effect::Notify {
                        chat: id,
                        message: Notice::AgentStopped,
                    });
                }
            }
            Input::SessionCreated {
                chat: id, session, ..
            } => {
                let chat = self.chats.get_mut(&id).expect("checked by is_current");
                chat.creating = false;
                if chat.session.is_none() {
                    chat.session = Some(session);
                }
                self.advance(id, &mut effects);
                // Stopped while the session was being made.
                self.retire_if_stale(id, &mut effects);
            }
            Input::SessionFailed {
                chat: id, error, ..
            } => {
                let chat = self.chats.get_mut(&id).expect("checked by is_current");
                chat.creating = false;
                if matches!(chat.turn, Turn::Waiting { .. }) {
                    chat.turn = Turn::Idle;
                    effects.push(Effect::Notify {
                        chat: id,
                        message: Notice::SessionFailed(error),
                    });
                }
                self.retire_if_stale(id, &mut effects);
            }
            Input::TurnEnded { agent, error, .. } => {
                let id = self.chat_of_agent(agent).expect("checked by is_current");
                let chat = self.chats.get_mut(&id).expect("chat_of_agent found it");
                if matches!(chat.turn, Turn::Running | Turn::Cancelling) {
                    chat.turn = Turn::Idle;
                    if let Some(error) = error {
                        effects.push(Effect::Notify {
                            chat: id,
                            message: Notice::TurnFailed(error),
                        });
                    }
                }
                if chat.baseline == Baseline::Ready {
                    effects.push(Effect::RefreshDiff { chat: id });
                }
                self.retire_if_stale(id, &mut effects);
            }
            Input::ToolsChanged => {
                self.stale_through = self.next_agent;
                let ids: Vec<_> = self.chats.keys().copied().collect();
                for id in ids {
                    self.retire_if_stale(id, &mut effects);
                }
            }
            Input::ToolFinished { agent, .. } => {
                let id = self.chat_of_agent(agent).expect("checked by is_current");
                if self.chats[&id].baseline == Baseline::Ready {
                    effects.push(Effect::RefreshDiff { chat: id });
                }
            }
            Input::BaselineTaken { chat: id, error } => {
                let Some(chat) = self.chats.get_mut(&id) else {
                    return effects;
                };
                if chat.baseline != Baseline::Taking {
                    return effects;
                }
                match error {
                    None => {
                        chat.baseline = Baseline::Ready;
                        effects.push(Effect::RefreshDiff { chat: id });
                    }
                    Some(error) => {
                        chat.baseline = Baseline::Unavailable;
                        effects.push(Effect::Notify {
                            chat: id,
                            message: Notice::BaselineFailed(error),
                        });
                    }
                }
            }
        }
        effects
    }

    /// Whether an input should be acted on. Agent events count only while
    /// their agent still belongs to the chat they concern.
    fn is_current(&self, input: &Input) -> bool {
        match input {
            Input::SessionCreated { agent, chat, .. }
            | Input::SessionFailed { agent, chat, .. } => self
                .chats
                .get(chat)
                .is_some_and(|c| c.agent == Some(*agent) && c.creating),
            Input::TurnEnded { agent, session, .. } | Input::ToolFinished { agent, session } => {
                self.chat_of_agent(*agent).is_some_and(|chat| {
                    self.chats[&chat].session.as_deref() == Some(session.as_str())
                })
            }
            Input::AgentExited { agent } => self.chat_of_agent(*agent).is_some(),
            _ => true,
        }
    }

    /// The properties every reachable state has. Checked after each step in
    /// debug builds and by the property tests.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut agents = std::collections::BTreeSet::new();
        for (id, chat) in &self.chats {
            if let Some(agent) = chat.agent {
                if !agents.insert(agent) {
                    return Err(format!("agent {agent} belongs to two chats"));
                }
                if agent > self.next_agent {
                    return Err(format!("chat {id} has an agent that was never started"));
                }
                if self.credential.is_none() {
                    return Err(format!("chat {id} kept its agent after sign-out"));
                }
            }
            if matches!(chat.turn, Turn::Running | Turn::Cancelling) {
                if chat.session.is_none() {
                    return Err(format!("chat {id} is running without a session"));
                }
                if chat.agent.is_none() {
                    return Err(format!("chat {id} is running without an agent"));
                }
            }
            if chat.creating && (chat.agent.is_none() || chat.session.is_some()) {
                return Err(format!("chat {id} is creating a session it can't get"));
            }
            if matches!(chat.turn, Turn::Waiting { .. })
                && chat.agent.is_some()
                && chat.session.is_none()
                && !chat.creating
            {
                return Err(format!("chat {id} waits for a session nobody is creating"));
            }
            if chat.turn != Turn::Idle && chat.baseline == Baseline::NotTaken {
                return Err(format!("chat {id} reached the agent without a baseline"));
            }
        }
        Ok(())
    }

    /// Starts the chat's agent if it has none and the user is signed in.
    fn ensure_agent(&mut self, id: ChatId, effects: &mut Vec<Effect>) {
        let Some(credential) = &self.credential else {
            return;
        };
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        if chat.agent.is_some() {
            return;
        }
        self.next_agent += 1;
        chat.agent = Some(self.next_agent);
        effects.push(Effect::StartAgent {
            agent: self.next_agent,
            chat: id,
            credential: credential.clone(),
        });
    }

    /// Moves a waiting prompt toward the agent: create the session if there
    /// is none, otherwise send the prompt.
    fn advance(&mut self, id: ChatId, effects: &mut Vec<Effect>) {
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        let (Some(agent), Turn::Waiting { prompt }) = (chat.agent, &chat.turn) else {
            return;
        };
        match &chat.session {
            Some(session) => {
                effects.push(Effect::Send {
                    agent,
                    command: Command::Prompt {
                        session: session.clone(),
                        cwd: chat.cwd.clone(),
                        prompt: prompt.clone(),
                    },
                });
                chat.turn = Turn::Running;
            }
            None if !chat.creating => {
                effects.push(Effect::Send {
                    agent,
                    command: Command::NewSession {
                        chat: id,
                        cwd: chat.cwd.clone(),
                    },
                });
                chat.creating = true;
            }
            None => {}
        }
    }

    /// Stops the chat's agent if it started before the tools last changed and
    /// nothing is under way with it: no turn, and no session being made,
    /// whose answer would be lost. Its next prompt starts a fresh one.
    fn retire_if_stale(&mut self, id: ChatId, effects: &mut Vec<Effect>) {
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        if let Some(agent) = chat.agent
            && agent <= self.stale_through
            && chat.turn == Turn::Idle
            && !chat.creating
        {
            effects.push(Effect::StopAgent { agent });
            chat.agent = None;
        }
    }

    /// Stops every chat's agent. Turns they were running can't finish;
    /// prompts that hadn't reached them wait for the next agent.
    fn stop_agents(&mut self, reason: Notice, effects: &mut Vec<Effect>) {
        for (id, chat) in &mut self.chats {
            let Some(agent) = chat.agent.take() else {
                continue;
            };
            chat.creating = false;
            effects.push(Effect::StopAgent { agent });
            if matches!(chat.turn, Turn::Running | Turn::Cancelling) {
                chat.turn = Turn::Idle;
                effects.push(Effect::Notify {
                    chat: *id,
                    message: reason.clone(),
                });
            }
        }
    }
}
