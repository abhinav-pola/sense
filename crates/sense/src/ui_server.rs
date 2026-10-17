//! `sense --ui-server`: the MCP server (over stdio) that gives fx the
//! `show_view` tool. It only checks the view and says so: the app draws it
//! from the tool call fx reports, which also keeps it in the session's
//! history.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::view::{self, View};

pub fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(reply) = answer(&message)
            && writeln!(stdout, "{reply}")
                .and_then(|_| stdout.flush())
                .is_err()
        {
            return;
        }
    }
}

/// The reply to one message, if it is a request.
pub fn answer(message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            // Whatever version the client asked for; the subset used here
            // is the same in all of them.
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": view::SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": [{
            "name": view::TOOL,
            "description": view::DESCRIPTION,
            "inputSchema": view::input_schema(),
        }]}),
        "tools/call" if message["params"]["name"] == view::TOOL => {
            match View::parse(&message["params"]["arguments"]) {
                Ok(view) => json!({"content": [{"type": "text",
                    "text": format!("Shown in the side panel as \"{}\".", view.title)}]}),
                Err(problem) => json!({"isError": true, "content": [{"type": "text",
                    "text": format!("Not shown: {problem}. Fix the view and call {} again.", view::TOOL)}]}),
            }
        }
        // Includes the newer protocol's `server/discover`: fx then falls
        // back to `initialize`.
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("Method not found: {method}")}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}
