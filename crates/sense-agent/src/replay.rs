//! Plays a recorded ACP session back as if it were fx.
//!
//! Fixtures are the JSONL files written with `SENSE_RECORD` against the real
//! API. The replayer walks the recording in order: each `recv` line is sent to
//! the client, and each `send` line waits for the client's next message, which
//! must use the same JSON-RPC method. Request ids differ between runs, so the
//! recorded ids are mapped to the live ones in both directions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use futures::StreamExt as _;
use futures::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use serde_json::Value;

#[derive(Clone, Copy)]
pub enum Pace {
    /// As fast as the client reads, for tests.
    Instant,
    /// The recorded gaps between lines, scaled by this factor, for perf runs.
    Recorded(f64),
}

static NEXT_INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct Fixture {
    records: Vec<Record>,
}

struct Record {
    ms: u64,
    from_client: bool,
    message: Value,
}

impl Fixture {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let mut records = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)?;
            let from_client = match record["dir"].as_str() {
                Some("send") => true,
                Some("recv") => false,
                _ => continue,
            };
            let raw = record["line"].as_str().context("record without a line")?;
            records.push(Record {
                ms: record["ms"].as_u64().unwrap_or(0),
                from_client,
                message: serde_json::from_str(raw)?,
            });
        }
        Ok(Self { records })
    }

    /// Serves the recording over a byte-stream pair. `incoming` carries what
    /// the client writes; `outgoing` is what the client reads.
    pub async fn serve(
        self,
        incoming: impl futures::AsyncRead + Unpin + Send + 'static,
        mut outgoing: impl futures::AsyncWrite + Unpin,
        pace: Pace,
    ) -> Result<()> {
        // The client is read continuously, not only when the recording
        // expects it to speak: a cancel can arrive at any point in a turn.
        let (client_tx, client) = async_channel::unbounded::<String>();
        smol::spawn(async move {
            let mut lines = BufReader::new(incoming).lines();
            while let Some(Ok(line)) = lines.next().await {
                if client_tx.send(line).await.is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut early = std::collections::VecDeque::new();
        // The live id of a prompt the recording hasn't answered yet.
        let mut open_prompt: Option<Value> = None;
        // Recorded client request id -> live client request id.
        let mut client_ids: HashMap<String, Value> = HashMap::new();
        // The recorded and live working folders, from session/new.
        let mut folders: Option<(PathBuf, PathBuf)> = None;
        let mut last_ms = 0;
        let mut sessions: Vec<String> = Vec::new();
        let instance = NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for record in self.records {
            if record.from_client {
                let line = match early.pop_front() {
                    Some(line) => line,
                    None => match client.recv().await {
                        Ok(line) => line,
                        Err(_) => bail!("client closed before the recording finished"),
                    },
                };
                let live: Value = serde_json::from_str(&line)?;
                if live["method"] == "session/cancel"
                    && record.message["method"] != "session/cancel"
                {
                    cancel(&mut outgoing, open_prompt.take()).await?;
                    break;
                }
                let expected = record.message.get("method").and_then(Value::as_str);
                let actual = live.get("method").and_then(Value::as_str);
                if expected != actual {
                    bail!("fixture expected {expected:?} from the client, got {actual:?}");
                }
                // A fixture is only as good as its match with today's client:
                // one recorded by a client that asked fx for different things
                // replays a conversation this client would never have.
                if expected == Some("initialize") {
                    let caps = |message: &Value| message["params"]["clientCapabilities"].clone();
                    if caps(&record.message) != caps(&live) {
                        bail!(
                            "fixture was recorded with different client capabilities; re-record it \
                             (recorded {}, now {})",
                            caps(&record.message),
                            caps(&live)
                        );
                    }
                }
                // What the user sent must have the recording's shape: the
                // same kinds of content, and the very same images, byte for
                // byte (text may differ; tests type their own).
                if expected == Some("session/prompt") {
                    let shape = |message: &Value| -> Vec<String> {
                        message["params"]["prompt"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|block| {
                                let data = block["data"].as_str().unwrap_or_default();
                                let mut hasher = std::hash::DefaultHasher::new();
                                std::hash::Hash::hash(data, &mut hasher);
                                format!(
                                    "{}{} ({} bytes, {:x})",
                                    block["type"],
                                    block["mimeType"],
                                    data.len(),
                                    std::hash::Hasher::finish(&hasher)
                                )
                            })
                            .collect()
                    };
                    if shape(&record.message) != shape(&live) {
                        bail!(
                            "the prompt sent {:?} where the recording has {:?}",
                            shape(&live),
                            shape(&record.message)
                        );
                    }
                }
                // A model switch must switch to the recording's model.
                if expected == Some("session/set_config_option") {
                    let setting = |message: &Value| {
                        (
                            message["params"]["configId"].clone(),
                            message["params"]["value"].clone(),
                        )
                    };
                    if setting(&record.message) != setting(&live) {
                        bail!(
                            "set {:?} where the recording sets {:?}",
                            setting(&live),
                            setting(&record.message)
                        );
                    }
                }
                if expected == Some("session/new") {
                    let cwd =
                        |message: &Value| message["params"]["cwd"].as_str().map(PathBuf::from);
                    folders = cwd(&record.message).zip(cwd(&live));
                }
                if expected == Some("session/prompt") {
                    open_prompt = live.get("id").cloned();
                }
                if let (Some(recorded), Some(live_id), Some(_)) =
                    (record.message.get("id"), live.get("id"), expected)
                {
                    client_ids.insert(recorded.to_string(), live_id.clone());
                }
                continue;
            }
            // A cancel sent while the agent is mid-turn acts at once; any
            // other early message (a client that doesn't wait for every
            // trailing update) waits for its place in the recording.
            while let Ok(line) = client.try_recv() {
                let live: Value = serde_json::from_str(&line)?;
                if live["method"] == "session/cancel" {
                    cancel(&mut outgoing, open_prompt.take()).await?;
                    while client.recv().await.is_ok() {}
                    return Ok(());
                }
                early.push_back(line);
            }
            if let Pace::Recorded(scale) = pace {
                let gap = record.ms.saturating_sub(last_ms) as f64 * scale;
                if gap >= 1.0 {
                    smol::Timer::after(Duration::from_millis(gap as u64)).await;
                }
            }
            last_ms = record.ms;
            let mut message = record.message;
            if let Some((recorded, live)) = &folders {
                replay_file_edits(&message, recorded, live)?;
            }
            // A response to a client request carries the client's live id.
            let live_id = message
                .get("method")
                .is_none()
                .then(|| message.get("id").map(Value::to_string))
                .flatten()
                .and_then(|id| client_ids.get(&id));
            if let Some(live_id) = live_id {
                if open_prompt.as_ref() == Some(live_id) {
                    open_prompt = None;
                }
                message["id"] = live_id.clone();
            }
            if let Some(session) = message["result"]["sessionId"].as_str() {
                sessions.push(session.to_string());
            }
            // Each replay is its own session, as each fx session would be:
            // recorded ids get a per-replay suffix wherever they appear.
            let mut line = message.to_string();
            // Paths name the live folder, as they would from fx running there.
            if let Some((recorded, live)) = &folders {
                // The /private form first: it contains the plain one.
                for recorded in [private(recorded), recorded.clone()] {
                    line =
                        line.replace(&recorded.display().to_string(), &live.display().to_string());
                }
            }
            for session in &sessions {
                line = line.replace(
                    &format!("\"{session}\""),
                    &format!("\"{session}-r{instance}\""),
                );
            }
            outgoing.write_all(format!("{line}\n").as_bytes()).await?;
            outgoing.flush().await?;
        }
        // Stay open like a live agent until the client hangs up.
        while client.recv().await.is_ok() {}
        Ok(())
    }
}

/// fx is not running during a replay, so the edits it made are redone here
/// from the `diff` content it reported, moved from the recorded working
/// folder to the live one. Anything that reads the disk then sees what it
/// would have seen live.
fn replay_file_edits(message: &Value, recorded: &Path, live: &Path) -> Result<()> {
    let update = &message["params"]["update"];
    if update["sessionUpdate"] != "tool_call_update" {
        return Ok(());
    }
    let diffs = update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "diff");
    for diff in diffs {
        let (Some(path), Some(text)) = (diff["path"].as_str(), diff["newText"].as_str()) else {
            continue;
        };
        let path = Path::new(path);
        // fx reports resolved paths, so /tmp arrives as /private/tmp.
        let relative = path
            .strip_prefix(recorded)
            .or_else(|_| path.strip_prefix(private(recorded)));
        let Ok(relative) = relative else {
            bail!(
                "recorded edit to {} is outside the recorded folder",
                path.display()
            );
        };
        let target = live.join(relative);
        // The recording only holds if the live file starts where the recorded
        // one did; otherwise the replay would invent a different change.
        let before = std::fs::read_to_string(&target).ok();
        if before.as_deref() != diff["oldText"].as_str() {
            bail!(
                "replayed edit to {} expects it to start as recorded; seed the folder first",
                target.display()
            );
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, text)
            .with_context(|| format!("replay edit to {}", target.display()))?;
    }
    Ok(())
}

/// The path as fx reports it on macOS, where /tmp and /var live in /private.
fn private(path: &Path) -> PathBuf {
    Path::new("/private").join(path.strip_prefix("/").unwrap_or(path))
}

/// What fx does on `session/cancel`: the open prompt ends, cancelled. The
/// rest of the recorded turn never happens.
async fn cancel(
    outgoing: &mut (impl futures::AsyncWrite + Unpin),
    prompt: Option<Value>,
) -> Result<()> {
    if let Some(id) = prompt {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "stopReason": "cancelled" },
        });
        outgoing
            .write_all(format!("{response}\n").as_bytes())
            .await?;
        outgoing.flush().await?;
    }
    Ok(())
}
