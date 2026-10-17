//! Replays fixtures recorded against the real OpenRouter API through fx.

use std::path::PathBuf;
use std::time::Duration;

use sense_agent::acp::{ContentBlock, SessionUpdate, ToolCallContent, ToolCallStatus};
use sense_agent::replay::Pace;
use sense_agent::{Command, Event, Launch};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/acp")
        .join(name)
}

#[test]
fn edit_file_turn_replays() {
    let agent = sense_agent::spawn(Launch::Replay {
        fixture: fixture("edit-file.jsonl"),
        pace: Pace::Instant,
        model: Some("anthropic/claude-sonnet-5.5".into()),
    });
    let workspace = std::env::temp_dir().join(format!("sense-replay-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).unwrap();
    // The folder as it was when the fixture was recorded.
    std::fs::write(
        workspace.join("calc.py"),
        "def add(a, b):\n    return a - b\n",
    )
    .unwrap();
    agent.send(Command::NewSession {
        cwd: workspace.clone(),
        tag: 7,
    });

    let mut text = String::new();
    let mut tools = Vec::new();
    let mut completed = 0;
    let mut diffs = Vec::new();
    let mut ended = false;
    smol::block_on(async {
        let deadline = smol::Timer::after(Duration::from_secs(10));
        let run = async {
            while let Ok(event) = agent.events.recv().await {
                match event {
                    Event::SessionCreated { tag, session } => {
                        assert_eq!(tag, 7);
                        agent.send(Command::Prompt {
                            session,
                            cwd: workspace.clone(),
                            text: "fix calc.py".into(),
                            images: Vec::new(),
                            model: None,
                        });
                    }
                    Event::Update {
                        update: SessionUpdate::AgentMessageChunk(chunk),
                        ..
                    } => {
                        if let ContentBlock::Text(chunk) = chunk.content {
                            text.push_str(&chunk.text);
                        }
                    }
                    Event::Update {
                        update: SessionUpdate::ToolCall(call),
                        ..
                    } => tools.push(call.title),
                    Event::Update {
                        update: SessionUpdate::ToolCallUpdate(update),
                        ..
                    } => {
                        if update.fields.status == Some(ToolCallStatus::Completed) {
                            completed += 1;
                        }
                        for item in update.fields.content.iter().flatten() {
                            if let ToolCallContent::Diff(diff) = item {
                                diffs.push(diff.clone());
                            }
                        }
                    }
                    Event::TurnEnded {
                        stop_reason, error, ..
                    } => {
                        assert_eq!(error, None);
                        assert!(stop_reason.is_some());
                        ended = true;
                        return;
                    }
                    Event::Error(err) => panic!("{err}"),
                    _ => {}
                }
            }
        };
        futures::future::select(Box::pin(run), deadline).await;
    });

    assert!(ended, "the turn never ended");
    // The model's exact steps vary between recordings; these hold for any.
    assert!(
        tools.iter().any(|title| title == "Editing"),
        "tools: {tools:?}"
    );
    assert_eq!(completed, tools.len());
    assert_eq!(diffs.len(), 1, "one edit, one diff");
    assert_eq!(
        diffs[0].path,
        workspace.join("calc.py"),
        "paths must name the live folder"
    );
    assert!(
        diffs[0]
            .old_text
            .as_deref()
            .unwrap_or_default()
            .contains("a - b")
    );
    assert!(diffs[0].new_text.contains("a + b"));
    assert!(text.contains("a + b"), "agent text: {text}");
    // The replayer redoes recorded edits in the live folder.
    let replayed = std::fs::read_to_string(workspace.join("calc.py")).unwrap();
    assert_eq!(replayed, "def add(a, b):\n    return a + b\n");
    std::fs::remove_dir_all(&workspace).unwrap();
}
