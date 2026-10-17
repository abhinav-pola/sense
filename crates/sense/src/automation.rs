//! A local control socket for driving the app without the OS input stack, so
//! tests and agents never touch the user's cursor, keyboard or focus.
//!
//! Enabled by the `automation` feature and `SENSE_AUTOMATION=<socket path>`.
//! Each line on the socket is one JSON command; each reply is one JSON line.
//!
//! ```text
//! {"cmd":"state"}
//! {"cmd":"screenshot","path":"/tmp/sense.png"}
//! {"cmd":"type","text":"hello"}     {"cmd":"submit"}
//! {"cmd":"new_chat"}                {"cmd":"toggle_panel"}
//! {"cmd":"answer_permission","choice":0}
//! {"cmd":"click","x":40,"y":620}    {"cmd":"key","keys":"cmd-n"}
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use async_channel::{Receiver, Sender};
use serde_json::Value;

pub struct Request {
    pub command: Value,
    pub reply: Sender<Value>,
}

/// Starts listening if `SENSE_AUTOMATION` is set; requests arrive on the
/// returned channel for the UI thread to handle.
pub fn listen() -> Option<Receiver<Request>> {
    let path = PathBuf::from(std::env::var_os("SENSE_AUTOMATION")?);
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("automation: cannot bind {}: {err}", path.display());
            return None;
        }
    };
    let (tx, rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("sense-automation".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut writer = &stream;
                    for line in BufReader::new(&stream).lines().map_while(Result::ok) {
                        let reply = match serde_json::from_str::<Value>(&line) {
                            Ok(command) => {
                                let (reply_tx, reply_rx) = async_channel::bounded(1);
                                if tx
                                    .send_blocking(Request {
                                        command,
                                        reply: reply_tx,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                reply_rx.recv_blocking().unwrap_or(Value::Null)
                            }
                            Err(err) => serde_json::json!({ "error": err.to_string() }),
                        };
                        if writeln!(writer, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        })
        .ok()?;
    Some(rx)
}
