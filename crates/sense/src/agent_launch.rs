//! Builds the `fx acp` launch: which binary, and the environment that points
//! it at OpenRouter with the signed-in user's key.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sense_agent::Launch;

pub const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-5.5";
const PROVIDER_LABEL: &str = "openrouter";
const KEY_ENV: &str = "SENSE_OPENROUTER_API_KEY";

/// `known` are the models a session may be switched to; fx sizes its
/// context and output to whichever it's on.
pub fn fx_launch(api_key: &str, model: &str, known: &[crate::models::Model]) -> Option<Launch> {
    let program = fx_binary()?;
    let mut metadata = serde_json::Map::new();
    // The default model's limits, for when the list hasn't been fetched.
    metadata.insert(
        model.to_string(),
        serde_json::json!({
            "context_window": 1_000_000,
            "max_output_tokens": 128_000,
            "supports_tool_use": true,
            "supports_vision": true,
        }),
    );
    for known in known {
        let mut limits = serde_json::json!({
            "context_window": known.context_window,
            "supports_tool_use": true,
            // The list holds only models that read images.
            "supports_vision": true,
        });
        if let Some(tokens) = known.max_output_tokens {
            limits["max_output_tokens"] = tokens.into();
        }
        metadata.insert(known.id.clone(), limits);
    }
    let providers = serde_json::json!({
        PROVIDER_LABEL: {
            "protocol": "openai-chat-completions",
            "base_url": "https://openrouter.ai/api/v1",
            "auth": { "type": "bearer", "env": KEY_ENV },
            "headers": {
                "HTTP-Referer": "https://github.com/abhinavpola/sense",
                "X-Title": "Sense",
            },
            // fx needs the context window to know when to compact, and
            // vision to send pasted and attached images.
            "model_metadata": metadata,
        }
    });
    let env = BTreeMap::from([
        ("FX_CONFIGURED_PROVIDERS".to_string(), providers.to_string()),
        ("FX_PROVIDER".to_string(), PROVIDER_LABEL.to_string()),
        ("FX_MODEL".to_string(), model.to_string()),
        (KEY_ENV.to_string(), api_key.to_string()),
    ]);
    Some(Launch::Process {
        program,
        args: vec!["acp".into()],
        env,
        record: std::env::var_os("SENSE_RECORD").map(PathBuf::from),
        views: Some(crate::view::tool()),
    })
}

/// `SENSE_REPLAY=<fixture>` swaps fx for a recorded session, so the app runs
/// offline with no key. `SENSE_REPLAY_PACE` scales the recorded timing; 0
/// replays instantly.
pub fn replay_launch() -> Option<Launch> {
    use sense_agent::replay::Pace;
    let fixture = std::env::var_os("SENSE_REPLAY")?;
    let scale = std::env::var("SENSE_REPLAY_PACE")
        .ok()
        .and_then(|pace| pace.parse::<f64>().ok())
        .unwrap_or(1.0);
    let pace = if scale > 0.0 {
        Pace::Recorded(scale)
    } else {
        Pace::Instant
    };
    Some(Launch::Replay {
        fixture: fixture.into(),
        pace,
        // Fixtures are recorded with the default model.
        model: Some(DEFAULT_MODEL.into()),
        views: Some(crate::view::tool()),
    })
}

/// `SENSE_FX` wins; otherwise the vendored build next to the workspace.
fn fx_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SENSE_FX") {
        return Some(path.into());
    }
    let vendored = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/fx/zig-out/bin/fx");
    vendored.exists().then_some(vendored)
}
