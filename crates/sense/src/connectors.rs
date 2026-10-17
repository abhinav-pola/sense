//! Connectors: the MCP servers the agent can use, such as Notion or Linear.
//!
//! They live in fx's own profile config, `~/.fx/mcp.json`, so the same file
//! serves fx in a terminal, `fx mcp add` writes to it, and fx keeps the
//! servers' sign-ins (in the Keychain) by name and address. The app asks fx
//! to load the file into every session (`sense-agent`'s
//! `profileMcpServers`), watches it, and on a change has each chat's agent
//! replaced once the chat is idle (`Input::ToolsChanged`): fx reads the file
//! when a session starts or resumes, so a fresh agent picks the change up.
//!
//! What a server is doing comes from `fx mcp list --connect`, which connects
//! to each one; signing in to one is `fx mcp auth NAME`, which opens the
//! browser and waits for it to come back.
//!
//! Both are started on the main thread and only waited for in the
//! background: fx started from one of the background executor's threads
//! stalls in `mcp list --connect` right after a server answers `initialize`,
//! and never finishes (the agents, started on the main thread, don't).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

/// `~/.fx/mcp.json`.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".fx/mcp.json"))
}

/// The file as it is now; `None` when there's none. Read whole: it's small,
/// and comparing the text catches every save, renames included.
pub fn read_config() -> Option<String> {
    std::fs::read_to_string(config_path()?).ok()
}

/// What `Edit connectors…` opens when there's no file yet: the two servers
/// asked for most, switched off until the user turns them on.
pub const STARTER: &str = r#"{
  "mcp": {
    "notion": {
      "type": "http",
      "url": "https://mcp.notion.com/mcp",
      "enabled": false
    },
    "linear": {
      "type": "http",
      "url": "https://mcp.linear.app/mcp",
      "enabled": false
    }
  }
}
"#;

/// The file, written with [`STARTER`] if it doesn't exist yet.
pub fn ensure_config() -> Result<PathBuf> {
    let path = config_path().context("no home folder")?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, STARTER)?;
    }
    Ok(path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ready {
        tools: Option<usize>,
    },
    NeedsSignIn,
    /// Switched off in the file (`"enabled": false`).
    Disabled,
    Connecting,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub status: Status,
}

impl Server {
    /// The menu's line for it.
    pub fn label(&self) -> String {
        let status = match &self.status {
            Status::Ready { tools: Some(1) } => "1 tool".to_string(),
            Status::Ready { tools: Some(n) } => format!("{n} tools"),
            Status::Ready { tools: None } => "connected".to_string(),
            Status::NeedsSignIn => "sign in…".to_string(),
            Status::Disabled => "off".to_string(),
            Status::Connecting => "connecting…".to_string(),
            Status::Failed(_) => "failed".to_string(),
        };
        format!("{} · {status}", self.name)
    }
}

/// Each server's state, from `fx mcp list --connect`: it connects to every
/// enabled server, so this takes a few seconds and needs the network.
pub fn status(fx: &Path) -> Result<Running> {
    start(fx, &["mcp", "list", "--connect"])
}

/// What [`status`] found, once fx has finished.
pub fn finish_status(running: Running) -> Result<Vec<Server>> {
    let (status, out, err) = running.wait()?;
    if !status.success() && !out.contains("MCP health") {
        bail!("{}", err.trim());
    }
    Ok(parse_list(&out))
}

/// An fx command under way.
pub struct Running(std::process::Child);

impl Running {
    /// Waits for it; blocks, so not on the main thread.
    pub fn wait(self) -> Result<(std::process::ExitStatus, String, String)> {
        let output = self.0.wait_with_output()?;
        Ok((
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

/// Starts fx; call it on the main thread (see the module docs).
fn start(fx: &Path, args: &[&str]) -> Result<Running> {
    let child = std::process::Command::new(fx)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("couldn't run fx")?;
    Ok(Running(child))
}

/// Signs in to `name`: fx opens the browser and finishes once it comes
/// back. Wait for it with [`finish_sign_in`].
pub fn sign_in(fx: &Path, name: &str) -> Result<Running> {
    start(fx, &["mcp", "auth", name])
}

pub fn finish_sign_in(running: Running) -> Result<()> {
    let (status, out, err) = running.wait()?;
    if !status.success() {
        bail!("{}", if err.trim().is_empty() { out } else { err }.trim());
    }
    Ok(())
}

/// Reads `fx mcp list`'s report: a line per server, `  NAME key=value …`,
/// then indented detail lines with its tool count and any failure.
pub fn parse_list(text: &str) -> Vec<Server> {
    let mut servers: Vec<Server> = Vec::new();
    for line in text.lines() {
        let fields = || {
            line.split_whitespace()
                .filter_map(|field| field.split_once('='))
        };
        let field = |key: &str| fields().find(|(k, _)| *k == key).map(|(_, v)| v);
        // A server's own line: two spaces, its name, then `source=`.
        if line.starts_with("  ") && !line.starts_with("   ") && field("source").is_some() {
            let Some(name) = line.split_whitespace().next() else {
                continue;
            };
            let status = match field("status").unwrap_or_default() {
                "ready" => Status::Ready { tools: None },
                "needs_auth" => Status::NeedsSignIn,
                "disabled" => Status::Disabled,
                "connecting" | "on_demand" | "unavailable" => Status::Connecting,
                other => Status::Failed(other.replace('_', " ")),
            };
            servers.push(Server {
                name: name.to_string(),
                status,
            });
            continue;
        }
        let Some(server) = servers.last_mut() else {
            continue;
        };
        if let (Status::Ready { tools }, Some(count)) = (&mut server.status, field("tools")) {
            *tools = count.parse().ok();
        }
        if let (Status::Failed(reason), Some(failure)) =
            (&mut server.status, line.trim().strip_prefix("failure="))
        {
            *reason = failure.to_string();
        }
    }
    servers
}

#[cfg(test)]
mod tests {
    use super::{Server, Status, parse_list};

    #[test]
    fn the_list_gives_each_server_its_state_tool_count_and_failure() {
        let text = "\
MCP health (4 servers):
  notion source=profile scope=profile policy=optional transport=http state=failed auth=required status=needs_auth
    negotiated_name=unavailable negotiated_version=unavailable protocol=unavailable
    tools=0 resources=0 templates=0 prompts=0 cache=unavailable subscription=unavailable
    failure=Authentication is required; run /mcp auth notion --open.
  linear source=profile scope=profile policy=optional transport=http state=ready auth=authenticated status=ready
    tools=23 resources=0 templates=0 prompts=0 cache=fresh subscription=unavailable
  local source=profile scope=profile policy=optional transport=stdio state=failed auth=none status=failed
    tools=0 resources=0 templates=0 prompts=0 cache=unavailable subscription=unavailable
    failure=spawn failed: no such file
  off source=profile scope=profile policy=optional transport=http state=disabled auth=none status=disabled
";
        assert_eq!(
            parse_list(text),
            [
                Server {
                    name: "notion".into(),
                    status: Status::NeedsSignIn
                },
                Server {
                    name: "linear".into(),
                    status: Status::Ready { tools: Some(23) }
                },
                Server {
                    name: "local".into(),
                    status: Status::Failed("spawn failed: no such file".into())
                },
                Server {
                    name: "off".into(),
                    status: Status::Disabled
                },
            ]
        );
        assert_eq!(parse_list("No MCP servers configured.\n"), []);
    }

    #[test]
    fn the_starter_file_is_one_fx_reads() {
        let value: serde_json::Value = serde_json::from_str(super::STARTER).unwrap();
        assert_eq!(value["mcp"]["notion"]["type"], "http");
        assert_eq!(value["mcp"]["linear"]["enabled"], false);
    }
}
