//! The models to pick from: OpenRouter's list of current models that read
//! text and images, write text, and hold a million tokens. Fetched only
//! when asked (Refresh models), and kept in the library between runs.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIST_URL: &str = "https://openrouter.ai/api/v1/models\
?output_modalities=text&input_modalities=text,image&context=1000000&max_age_days=150";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// OpenRouter's id, as sent to it: `anthropic/claude-sonnet-5.5`.
    pub id: String,
    pub name: String,
    pub context_window: u32,
    pub max_output_tokens: Option<u32>,
}

/// The list as of the app's build (scripts/update-models), for before the
/// first refresh.
pub fn seed() -> Vec<Model> {
    serde_json::from_str(include_str!("../assets/models.json"))
        .ok()
        .and_then(|body| parse(&body).ok())
        .unwrap_or_default()
}

/// Fetches the list. Blocks on the network: call it off the main thread.
pub fn fetch() -> Result<Vec<Model>> {
    let body: Value = ureq::get(LIST_URL)
        .call()
        .context("ask OpenRouter for its models")?
        .body_mut()
        .read_json()
        .context("read OpenRouter's model list")?;
    parse(&body)
}

/// The models an agent can run on: ones that call tools. Batch variants
/// answer hours later, so they're left out.
pub fn parse(body: &Value) -> Result<Vec<Model>> {
    let data = body["data"]
        .as_array()
        .context("no `data` in the model list")?;
    Ok(data
        .iter()
        .filter(|model| {
            model["supported_parameters"]
                .as_array()
                .is_some_and(|params| params.iter().any(|p| p == "tools"))
        })
        .filter_map(|model| {
            let id = model["id"].as_str()?;
            if id.ends_with(":batch") {
                return None;
            }
            let context_window = u32::try_from(model["context_length"].as_u64()?).ok()?;
            let max_output_tokens = model["top_provider"]["max_completion_tokens"]
                .as_u64()
                .and_then(|tokens| u32::try_from(tokens).ok())
                // fx takes an output limit only below the window.
                .filter(|tokens| *tokens < context_window);
            Some(Model {
                id: id.to_string(),
                name: model["name"].as_str().unwrap_or(id).to_string(),
                context_window,
                max_output_tokens,
            })
        })
        .collect())
}

/// A model's name as shown: from the list, or else its id without the
/// provider.
pub fn label(models: &[Model], id: &str) -> String {
    match models.iter().find(|model| model.id == id) {
        Some(model) => model.short_name().to_string(),
        None => id.split_once('/').map_or(id, |(_, name)| name).to_string(),
    }
}

impl Model {
    /// The name without its provider: "Claude Sonnet 5.5".
    pub fn short_name(&self) -> &str {
        self.name
            .split_once(": ")
            .map_or(&self.name, |(_, name)| name)
    }
}

#[cfg(test)]
mod tests {
    use super::{Model, parse};

    /// A slice of the real list (fixtures/openrouter-models.json): models
    /// with and without tools, a batch variant, and one with no output
    /// limit.
    #[test]
    fn keeps_the_models_an_agent_can_use_as_listed() {
        let body =
            serde_json::from_str(include_str!("../../../fixtures/openrouter-models.json")).unwrap();
        let models = parse(&body).unwrap();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(!ids.iter().any(|id| id.ends_with(":batch")), "{ids:?}");
        assert!(
            !ids.contains(&"scaledown/summarize"),
            "no tools, no agent: {ids:?}"
        );
        assert!(ids.contains(&"anthropic/claude-sonnet-5.5"), "{ids:?}");
        let sonnet = models
            .iter()
            .find(|m| m.id == "anthropic/claude-sonnet-5.5")
            .unwrap();
        assert_eq!(
            *sonnet,
            Model {
                id: "anthropic/claude-sonnet-5.5".into(),
                name: "Anthropic: Claude Sonnet 5.5".into(),
                context_window: 1_000_000,
                max_output_tokens: Some(128_000),
            }
        );
        assert_eq!(sonnet.short_name(), "Claude Sonnet 5.5");
        let router = models
            .iter()
            .find(|m| m.id == "typesafe/jev-router")
            .unwrap();
        assert_eq!(router.max_output_tokens, None);
    }

    /// The snapshot built into the app reads, and offers the default model.
    #[test]
    fn the_built_in_list_has_the_default_model() {
        let seed = super::seed();
        assert!(seed.len() > 10, "{}", seed.len());
        assert!(
            seed.iter()
                .any(|m| m.id == crate::agent_launch::DEFAULT_MODEL)
        );
    }

    #[test]
    fn a_body_without_a_list_is_an_error() {
        assert!(parse(&serde_json::json!({"error": "nope"})).is_err());
    }
}
