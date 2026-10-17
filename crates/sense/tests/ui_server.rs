//! `sense --ui-server`, the panel tools' MCP server, as fx talks to it.

use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// The real binary, driven the way fx drives a stdio MCP server: the newer
/// handshake refused, the older one accepted, the tool listed, and a good
/// and a bad call answered.
#[test]
fn speaks_mcp_over_stdio_as_fx_expects() {
    let mut server = Command::new(env!("CARGO_BIN_EXE_sense"))
        .arg("--ui-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = server.stdin.take().unwrap();
    let mut output = BufReader::new(server.stdout.take().unwrap());
    let mut ask = |input: &mut std::process::ChildStdin, message: serde_json::Value| {
        writeln!(input, "{message}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };

    let discover = ask(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 0, "method": "server/discover", "params": {}}),
    );
    assert_eq!(discover["error"]["code"], -32601);
    let init = ask(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fx"}}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    // A notification gets no reply: the next line answers the list.
    writeln!(
        input,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    let list = ask(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(list["id"], 2);
    assert_eq!(list["result"]["tools"][0]["name"], "show_view");

    let shown = ask(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "show_view",
            "arguments": {"id": "v", "title": "Results", "blocks": [{"type": "markdown", "text": "x"}]}}}),
    );
    assert_eq!(shown["result"]["isError"], json!(null));
    assert!(
        shown["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Results")
    );

    let refused = ask(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "show_view",
            "arguments": {"id": "v", "title": "t", "blocks": [{"type": "progress", "label": "x", "value": 9}]}}}),
    );
    assert_eq!(refused["result"]["isError"], true);
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("outside 0 to 1")
    );

    drop(input);
    assert!(server.wait().unwrap().success());
}
