//! Views the agent draws in the side panel: a small declarative layout the
//! model fills in through the `show_view` tool. The same types check a
//! call in the tool server (so the model hears what's wrong and retries)
//! and lay out the panel in the app.

use serde::Deserialize;
use serde_json::Value;

/// The MCP server's name, as fx reports it on each of its tool calls.
pub const SERVER: &str = "sense";
pub const TOOL: &str = "show_view";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    /// Showing a view with an id already shown replaces it in place.
    pub id: String,
    pub title: String,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Block {
    Markdown {
        text: String,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    Stats {
        items: Vec<Stat>,
    },
    BarChart {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        bars: Vec<Bar>,
    },
    Checklist {
        items: Vec<Check>,
    },
    Progress {
        label: String,
        /// From 0 to 1.
        value: f64,
    },
    Code {
        #[serde(default)]
        language: Option<String>,
        text: String,
    },
    Callout {
        tone: Tone,
        text: String,
    },
    /// Buttons that send their message to the agent as the user's reply.
    Actions {
        buttons: Vec<Action>,
    },
    /// Side by side: each column is its own list of blocks.
    Columns {
        columns: Vec<Vec<Block>>,
    },
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stat {
    pub label: String,
    pub value: Value,
    #[serde(default)]
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bar {
    pub label: String,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub text: String,
    pub done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub label: String,
    /// Sent to the agent, as if the user typed it, when clicked.
    pub message: String,
}

impl View {
    /// Reads a `show_view` call's arguments, saying what's wrong if they
    /// aren't a view: the tool server hands that back to the model.
    pub fn parse(arguments: &Value) -> Result<Self, String> {
        // fx's own tools take their arguments as `{"request": ...}`, and
        // models carry the habit over: the view may come wrapped that way,
        // as an object or as JSON text.
        let unwrapped;
        let arguments = match arguments
            .as_object()
            .filter(|args| args.len() == 1)
            .and_then(|args| args.get("request"))
        {
            Some(Value::String(text)) => {
                unwrapped = serde_json::from_str::<Value>(text).map_err(|err| err.to_string())?;
                &unwrapped
            }
            Some(inner @ Value::Object(_)) => inner,
            _ => arguments,
        };
        let view = Self::deserialize(arguments).map_err(|err| err.to_string())?;
        if view.id.trim().is_empty() {
            return Err("`id` must not be empty".into());
        }
        let mut problems = Vec::new();
        check_blocks(&view.blocks, &mut problems);
        match problems.first() {
            Some(problem) => Err(problem.clone()),
            None => Ok(view),
        }
    }
}

fn check_blocks(blocks: &[Block], problems: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Table { columns, rows } => {
                if let Some(row) = rows.iter().find(|row| row.len() != columns.len()) {
                    problems.push(format!(
                        "a table row has {} cells for {} columns",
                        row.len(),
                        columns.len()
                    ));
                }
            }
            Block::Progress { value, .. } if !(0.0..=1.0).contains(value) => {
                problems.push(format!("progress `value` {value} is outside 0 to 1"));
            }
            Block::BarChart { bars, .. } if bars.iter().any(|bar| !bar.value.is_finite()) => {
                problems.push("a bar's `value` isn't a number".into());
            }
            Block::Columns { columns } => {
                for column in columns {
                    check_blocks(column, problems);
                }
            }
            _ => {}
        }
    }
}

/// A table cell or stat value as shown: strings as they are, anything else
/// as JSON.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// What the model is told about the tool.
pub const DESCRIPTION: &str = "Show a view in the side panel next to the chat, \
for anything easier to take in visually than as chat text: comparisons \
(table), numbers (stats, bar_chart), a plan you are working through \
(checklist, updated as you go), progress, or choices for the user (actions: \
each button sends its message back to you as the user's reply). Reusing an \
`id` replaces that view, so keep one id per subject and update it as things \
change. Keep the chat reply short when the view says it.";

/// The tool's input schema (JSON Schema), mirroring [`View`].
pub fn input_schema() -> Value {
    let block = serde_json::json!({
        "oneOf": [
            {"type": "object", "required": ["type", "text"], "properties": {
                "type": {"const": "markdown"}, "text": {"type": "string"}}},
            {"type": "object", "required": ["type", "columns", "rows"], "properties": {
                "type": {"const": "table"},
                "columns": {"type": "array", "items": {"type": "string"}},
                "rows": {"type": "array", "items": {"type": "array"},
                         "description": "One cell per column."}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "stats"},
                "items": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": {"type": "string"}, "value": {},
                                   "hint": {"type": "string"}}}}}},
            {"type": "object", "required": ["type", "bars"], "properties": {
                "type": {"const": "bar_chart"}, "title": {"type": "string"},
                "unit": {"type": "string"},
                "bars": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": {"type": "string"}, "value": {"type": "number"}}}}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "checklist"},
                "items": {"type": "array", "items": {"type": "object", "required": ["text", "done"],
                    "properties": {"text": {"type": "string"}, "done": {"type": "boolean"}}}}}},
            {"type": "object", "required": ["type", "label", "value"], "properties": {
                "type": {"const": "progress"}, "label": {"type": "string"},
                "value": {"type": "number", "minimum": 0, "maximum": 1}}},
            {"type": "object", "required": ["type", "text"], "properties": {
                "type": {"const": "code"}, "language": {"type": "string"}, "text": {"type": "string"}}},
            {"type": "object", "required": ["type", "tone", "text"], "properties": {
                "type": {"const": "callout"},
                "tone": {"enum": ["info", "success", "warning", "error"]}, "text": {"type": "string"}}},
            {"type": "object", "required": ["type", "buttons"], "properties": {
                "type": {"const": "actions"},
                "buttons": {"type": "array", "items": {"type": "object", "required": ["label", "message"],
                    "properties": {"label": {"type": "string"}, "message": {"type": "string"}}}}}},
            {"type": "object", "required": ["type", "columns"], "properties": {
                "type": {"const": "columns"},
                "columns": {"type": "array", "items": {"type": "array", "items": {"$ref": "#/$defs/block"}},
                            "description": "Each column is a list of blocks, shown side by side."}}}
        ]
    });
    serde_json::json!({
        "type": "object",
        "required": ["id", "title", "blocks"],
        "properties": {
            "id": {"type": "string", "description": "Stable id; showing the same id again replaces the view."},
            "title": {"type": "string"},
            "blocks": {"type": "array", "items": {"$ref": "#/$defs/block"}}
        },
        "$defs": {"block": block}
    })
}

#[cfg(test)]
mod tests {
    use super::{Block, Tone, View, input_schema};
    use serde_json::json;

    #[test]
    fn a_view_with_every_kind_of_block_reads_as_written() {
        let view = View::parse(&json!({
            "id": "plan", "title": "Plan",
            "blocks": [
                {"type": "markdown", "text": "**hi**"},
                {"type": "table", "columns": ["a", "b"], "rows": [["x", 1]]},
                {"type": "stats", "items": [{"label": "p95", "value": "12 ms", "hint": "-3%"}]},
                {"type": "bar_chart", "bars": [{"label": "a", "value": 2.5}], "unit": "ms"},
                {"type": "checklist", "items": [{"text": "read", "done": true}]},
                {"type": "progress", "label": "tests", "value": 0.5},
                {"type": "code", "language": "rust", "text": "fn main() {}"},
                {"type": "callout", "tone": "warning", "text": "careful"},
                {"type": "actions", "buttons": [{"label": "Go", "message": "go ahead"}]},
                {"type": "columns", "columns": [[{"type": "markdown", "text": "l"}], []]}
            ]
        }))
        .unwrap();
        assert_eq!(view.blocks.len(), 10);
        assert_eq!(
            view.blocks[7],
            Block::Callout {
                tone: Tone::Warning,
                text: "careful".into()
            }
        );
        assert!(matches!(&view.blocks[9], Block::Columns { columns } if columns.len() == 2));
    }

    /// The shape fx's own tools use, which models carry over; recorded from
    /// a real call (fixtures/acp/view.jsonl).
    #[test]
    fn a_view_wrapped_as_a_request_reads_the_same() {
        let plain = json!({"id": "v", "title": "t", "blocks": [{"type": "markdown", "text": "x"}]});
        let expected = View::parse(&plain).unwrap();
        assert_eq!(
            View::parse(&json!({"request": plain.clone()})).unwrap(),
            expected
        );
        assert_eq!(
            View::parse(&json!({"request": plain.to_string()})).unwrap(),
            expected
        );
        assert!(View::parse(&json!({"request": "{ not json"})).is_err());
    }

    #[test]
    fn mistakes_are_named_so_the_model_can_fix_them() {
        let cases = [
            (
                json!({"id": "v", "title": "t", "blocks": [{"type": "pie"}]}),
                "pie",
            ),
            (
                json!({"id": "v", "title": "t", "blocks": [
                {"type": "table", "columns": ["a", "b"], "rows": [["only one"]]}]}),
                "1 cells for 2 columns",
            ),
            (
                json!({"id": "v", "title": "t", "blocks": [
                {"type": "progress", "label": "x", "value": 40}]}),
                "outside 0 to 1",
            ),
            (json!({"id": " ", "title": "t", "blocks": []}), "`id`"),
            (json!({"id": "v", "blocks": []}), "title"),
            (
                json!({"id": "v", "title": "t", "blocks": [
                {"type": "columns", "columns": [[{"type": "progress", "label": "x", "value": -1}]]}]}),
                "outside 0 to 1",
            ),
            (
                json!({"id": "v", "title": "t", "blocks": [], "colour": "red"}),
                "colour",
            ),
        ];
        for (arguments, expected) in cases {
            let err = View::parse(&arguments).unwrap_err();
            assert!(err.contains(expected), "{arguments}: {err}");
        }
    }

    /// The schema the model sees names every block type the parser takes,
    /// and no others.
    #[test]
    fn the_schema_offers_exactly_the_blocks_that_parse() {
        let schema = input_schema();
        let offered: Vec<String> = schema["$defs"]["block"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| {
                block["properties"]["type"]["const"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(offered.len(), 10);
        for kind in &offered {
            // Each offered type gets past the type check: any error is
            // about its fields, not an unknown variant.
            let err = View::parse(&json!({"id": "v", "title": "t", "blocks": [{"type": kind}]}))
                .unwrap_err();
            assert!(!err.contains("unknown variant"), "{kind}: {err}");
        }
        let err = View::parse(&json!({"id": "v", "title": "t", "blocks": [{"type": "nope"}]}))
            .unwrap_err();
        assert!(err.contains("unknown variant"), "{err}");
    }
}
