//! Views the agent draws in the side panel: a small declarative layout the
//! model fills in through fx's `show_view` tool. The same types check each
//! call (so the model hears what's wrong and retries) and lay out the panel.
//!
//! fx owns the tool; this app owns its format. The description and schema go
//! to fx at initialize ([`tool`]), fx sends each call back to be checked, and
//! the panel draws the view from the tool call fx reports, which also keeps
//! it in the session's history.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

/// The tool's name, as fx reports it on each call.
pub const TOOL: &str = "show_view";
/// The MCP server sessions used before fx drew views itself; their saved
/// history names calls by it.
pub const LEGACY_SERVER: &str = "sense";

/// Graphs larger than this are no longer readable in a side panel.
const MAX_GRAPH_NODES: usize = 80;
const MAX_GRAPH_EDGES: usize = 240;
/// Either side of a diff block.
const MAX_DIFF_BYTES: usize = 256 * 1024;

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
    /// One line per series over shared x labels.
    LineChart {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        x: Vec<String>,
        series: Vec<Series>,
    },
    /// As a line chart, with the area under each series filled.
    AreaChart {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        x: Vec<String>,
        series: Vec<Series>,
    },
    PieChart {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        /// A ring rather than a full disc.
        #[serde(default)]
        donut: bool,
        slices: Vec<Slice>,
    },
    /// Each series as a polygon over the same axes.
    RadarChart {
        #[serde(default)]
        title: Option<String>,
        axes: Vec<String>,
        series: Vec<Series>,
        /// The value at the rim; the largest value when not given.
        #[serde(default)]
        max: Option<f64>,
    },
    /// Quantities flowing between named stages.
    Sankey {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        unit: Option<String>,
        links: Vec<Flow>,
    },
    /// Boxes and arrows, laid out automatically: flowcharts, architecture,
    /// state machines, dependency graphs.
    Graph {
        #[serde(default)]
        direction: Direction,
        nodes: Vec<Node>,
        edges: Vec<Edge>,
    },
    Tree {
        items: Vec<TreeItem>,
    },
    /// A sequence with each step's status.
    Steps {
        items: Vec<Step>,
    },
    /// One file's change, before and after.
    Diff {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        language: Option<String>,
        before: String,
        after: String,
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
    Tags {
        items: Vec<Tag>,
    },
    KeyValue {
        items: Vec<Pair>,
    },
    /// Buttons that send their message to the agent as the user's reply.
    Actions {
        buttons: Vec<Action>,
    },
    /// Side by side: each column is its own list of blocks.
    Columns {
        columns: Vec<Vec<Block>>,
    },
    /// One pane at a time, picked by its label.
    Tabs {
        tabs: Vec<Pane>,
    },
    /// Sections that fold open and closed.
    Accordion {
        sections: Vec<Section>,
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
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slice {
    pub label: String,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub from: String,
    pub to: String,
    pub value: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Edges point down the panel.
    #[default]
    Down,
    /// Edges point across the panel.
    Right,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    /// Shown in the box; the id when not given.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub tone: Option<Tone>,
}

impl Node {
    pub fn text(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeItem {
    pub label: String,
    /// Dimmed text after the label, such as a size or a status.
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub children: Vec<TreeItem>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub status: StepStatus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Done,
    Active,
    #[default]
    Pending,
    Failed,
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
pub struct Tag {
    pub label: String,
    /// Neutral when not given.
    #[serde(default)]
    pub tone: Option<Tone>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub label: String,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub label: String,
    /// Sent to the agent, as if the user typed it, when clicked.
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pane {
    pub label: String,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    pub blocks: Vec<Block>,
    /// Open when first shown.
    #[serde(default)]
    pub open: bool,
}

impl Block {
    /// The blocks inside this one, in the order they are laid out: every
    /// column, every tab, every section, shown or not.
    pub fn children(&self) -> Vec<&[Block]> {
        match self {
            Block::Columns { columns } => columns.iter().map(Vec::as_slice).collect(),
            Block::Tabs { tabs } => tabs.iter().map(|pane| pane.blocks.as_slice()).collect(),
            Block::Accordion { sections } => sections
                .iter()
                .map(|section| section.blocks.as_slice())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// How many blocks `blocks` holds, nested ones included.
pub fn count(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| 1 + block.children().into_iter().map(count).sum::<usize>())
        .sum()
}

impl View {
    /// Reads a `show_view` call's arguments, saying what's wrong if they
    /// aren't a view: fx hands that back to the model.
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
        match problems.into_iter().next() {
            Some(problem) => Err(problem),
            None => Ok(view),
        }
    }
}

/// What the model reads back from a call: what was shown, or what to fix.
pub fn check(arguments: &Value) -> Result<String, String> {
    match View::parse(arguments) {
        Ok(view) => Ok(format!("Shown in the side panel as \"{}\".", view.title)),
        Err(problem) => Err(format!(
            "Not shown: {problem}. Fix the view and call {TOOL} again."
        )),
    }
}

/// fx's `show_view` tool, in this app's format.
pub fn tool() -> sense_agent::Views {
    sense_agent::Views {
        description: DESCRIPTION.into(),
        input_schema: input_schema(),
        check: std::sync::Arc::new(check),
    }
}

fn check_blocks(blocks: &[Block], problems: &mut Vec<String>) {
    for block in blocks {
        check_block(block, problems);
        for children in block.children() {
            check_blocks(children, problems);
        }
    }
}

fn check_block(block: &Block, problems: &mut Vec<String>) {
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
        Block::LineChart { x, series, .. } | Block::AreaChart { x, series, .. } => {
            let kind = if matches!(block, Block::LineChart { .. }) {
                "line_chart"
            } else {
                "area_chart"
            };
            if x.is_empty() {
                problems.push(format!("a {kind} needs at least one `x` label"));
            } else if series.is_empty() {
                problems.push(format!("a {kind} needs at least one series"));
            }
            check_series(kind, "`x` labels", x.len(), series, problems);
        }
        Block::PieChart { slices, .. } => {
            if slices.is_empty() {
                problems.push("a pie_chart needs at least one slice".into());
            } else if let Some(slice) = slices
                .iter()
                .find(|slice| !slice.value.is_finite() || slice.value < 0.)
            {
                problems.push(format!(
                    "pie slice \"{}\" has `value` {}; slices take 0 or more",
                    slice.label, slice.value
                ));
            } else if slices.iter().all(|slice| slice.value == 0.) {
                problems.push("every pie slice is 0, so there is nothing to draw".into());
            }
        }
        Block::RadarChart {
            axes, series, max, ..
        } => {
            if axes.len() < 3 {
                problems.push(format!(
                    "a radar_chart needs at least 3 axes, not {}",
                    axes.len()
                ));
            } else if series.is_empty() {
                problems.push("a radar_chart needs at least one series".into());
            }
            check_series("radar_chart", "axes", axes.len(), series, problems);
            if let Some(max) = max
                && !(max.is_finite() && *max > 0.)
            {
                problems.push(format!("radar_chart `max` {max} must be more than 0"));
            }
        }
        Block::Sankey { links, .. } => check_sankey(links, problems),
        Block::Graph { nodes, edges, .. } => check_graph(nodes, edges, problems),
        Block::Diff { before, after, .. } => {
            if before.len() > MAX_DIFF_BYTES || after.len() > MAX_DIFF_BYTES {
                problems.push(format!(
                    "a diff side is over {} KiB; show the part that matters",
                    MAX_DIFF_BYTES / 1024
                ));
            }
        }
        Block::Tabs { tabs } if tabs.is_empty() => {
            problems.push("`tabs` needs at least one tab".into());
        }
        Block::Accordion { sections } if sections.is_empty() => {
            problems.push("an accordion needs at least one section".into());
        }
        _ => {}
    }
}

fn check_series(
    kind: &str,
    along: &str,
    expected: usize,
    series: &[Series],
    problems: &mut Vec<String>,
) {
    for one in series {
        if one.values.len() != expected {
            problems.push(format!(
                "{kind} series \"{}\" has {} values for {expected} {along}",
                one.name,
                one.values.len()
            ));
        } else if one.values.iter().any(|value| !value.is_finite()) {
            problems.push(format!(
                "{kind} series \"{}\" has a value that isn't a number",
                one.name
            ));
        }
    }
}

fn check_sankey(links: &[Flow], problems: &mut Vec<String>) {
    if links.is_empty() {
        problems.push("a sankey needs at least one link".into());
        return;
    }
    if let Some(link) = links
        .iter()
        .find(|link| !(link.value.is_finite() && link.value > 0.))
    {
        problems.push(format!(
            "sankey link \"{}\" → \"{}\" has `value` {}; links take more than 0",
            link.from, link.to, link.value
        ));
        return;
    }
    if let Some(link) = links.iter().find(|link| link.from == link.to) {
        problems.push(format!(
            "sankey link \"{}\" → \"{}\" flows into itself",
            link.from, link.to
        ));
        return;
    }
    let edges: Vec<(&str, &str)> = links
        .iter()
        .map(|link| (link.from.as_str(), link.to.as_str()))
        .collect();
    if let Some(stage) = find_cycle(&edges) {
        problems.push(format!(
            "sankey links loop back through \"{stage}\"; flows must only go forward"
        ));
    }
}

fn check_graph(nodes: &[Node], edges: &[Edge], problems: &mut Vec<String>) {
    if nodes.is_empty() {
        problems.push("a graph needs at least one node".into());
        return;
    }
    if nodes.len() > MAX_GRAPH_NODES || edges.len() > MAX_GRAPH_EDGES {
        problems.push(format!(
            "a graph takes at most {MAX_GRAPH_NODES} nodes and {MAX_GRAPH_EDGES} edges; split it up"
        ));
        return;
    }
    let mut ids = HashSet::new();
    for node in nodes {
        if node.id.trim().is_empty() {
            problems.push("a graph node has an empty `id`".into());
            return;
        }
        if !ids.insert(node.id.as_str()) {
            problems.push(format!("graph node id \"{}\" is used twice", node.id));
            return;
        }
    }
    for edge in edges {
        for end in [&edge.from, &edge.to] {
            if !ids.contains(end.as_str()) {
                problems.push(format!(
                    "graph edge \"{}\" → \"{}\" names \"{end}\", which isn't a node id",
                    edge.from, edge.to
                ));
                return;
            }
        }
    }
}

/// A node on a cycle among `edges`, if they have one.
fn find_cycle<'a>(edges: &[(&'a str, &'a str)]) -> Option<&'a str> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in edges {
        out.entry(from).or_default().push(to);
    }
    // 1: on the current path, 2: finished.
    let mut state: HashMap<&str, u8> = HashMap::new();
    fn visit<'a>(
        node: &'a str,
        out: &HashMap<&'a str, Vec<&'a str>>,
        state: &mut HashMap<&'a str, u8>,
    ) -> Option<&'a str> {
        match state.get(node) {
            Some(1) => return Some(node),
            Some(_) => return None,
            None => {}
        }
        state.insert(node, 1);
        for next in out.get(node).into_iter().flatten() {
            if let Some(found) = visit(next, out, state) {
                return Some(found);
            }
        }
        state.insert(node, 2);
        None
    }
    let mut starts: Vec<&str> = edges.iter().map(|(from, _)| *from).collect();
    starts.dedup();
    starts
        .into_iter()
        .find_map(|start| visit(start, &out, &mut state))
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
for anything easier to take in visually than as chat text. Blocks: markdown, \
table, stats, tags, key_value; charts (bar_chart, line_chart, area_chart, \
pie_chart, radar_chart, sankey for flows); graph for boxes and arrows \
(flowcharts, architecture, state machines, dependencies; laid out for you, \
so give nodes and edges, not coordinates); tree for hierarchies; steps for a \
sequence with status; diff for a change to one file; checklist (a plan you \
update as you go), progress, code, callout; actions (each button sends its \
message back to you as the user's reply); and columns, tabs and accordion to \
arrange other blocks. The arguments are the view itself (`id`, `title`, \
`blocks`), not wrapped in a `request`. Reusing an `id` replaces that view, so \
keep one id per subject and update it as things change. Keep the chat reply \
short when the view says it.";

/// The tool's input schema (JSON Schema), mirroring [`View`].
pub fn input_schema() -> Value {
    use serde_json::json;
    let string = json!({"type": "string"});
    let tone = json!({"enum": ["info", "success", "warning", "error"]});
    let series = json!({"type": "array", "items": {"type": "object", "required": ["name", "values"],
        "properties": {"name": string, "values": {"type": "array", "items": {"type": "number"}}}}});
    let blocks = json!({"type": "array", "items": {"$ref": "#/$defs/block"}});
    let xy_chart = |kind: &str, what: &str| {
        json!({"type": "object", "required": ["type", "x", "series"], "description": what,
            "properties": {
                "type": {"const": kind}, "title": string, "unit": string,
                "x": {"type": "array", "items": string, "description": "Labels along the x axis."},
                "series": {"description": "One value per x label in each series.", "type": "array",
                    "items": series["items"]}}})
    };
    let block = json!({
        "oneOf": [
            {"type": "object", "required": ["type", "text"], "properties": {
                "type": {"const": "markdown"}, "text": string}},
            {"type": "object", "required": ["type", "columns", "rows"], "properties": {
                "type": {"const": "table"},
                "columns": {"type": "array", "items": string},
                "rows": {"type": "array", "items": {"type": "array"},
                         "description": "One cell per column."}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "stats"},
                "items": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": string, "value": {}, "hint": string}}}}},
            {"type": "object", "required": ["type", "bars"], "properties": {
                "type": {"const": "bar_chart"}, "title": string, "unit": string,
                "bars": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": string, "value": {"type": "number"}}}}}},
            xy_chart("line_chart", "Lines over shared x labels, one per series."),
            xy_chart("area_chart", "As line_chart, with the area under each series filled."),
            {"type": "object", "required": ["type", "slices"], "properties": {
                "type": {"const": "pie_chart"}, "title": string, "unit": string,
                "donut": {"type": "boolean"},
                "slices": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": string, "value": {"type": "number", "minimum": 0}}}}}},
            {"type": "object", "required": ["type", "axes", "series"], "properties": {
                "type": {"const": "radar_chart"}, "title": string,
                "axes": {"type": "array", "items": string, "minItems": 3},
                "series": {"description": "One value per axis in each series.", "type": "array",
                    "items": series["items"]},
                "max": {"type": "number", "description": "Value at the rim; the largest value if omitted."}}},
            {"type": "object", "required": ["type", "links"], "description": "Flows between stages; must not loop.",
             "properties": {
                "type": {"const": "sankey"}, "title": string, "unit": string,
                "links": {"type": "array", "items": {"type": "object", "required": ["from", "to", "value"],
                    "properties": {"from": string, "to": string, "value": {"type": "number", "exclusiveMinimum": 0}}}}}},
            {"type": "object", "required": ["type", "nodes", "edges"],
             "description": "Boxes and arrows, laid out automatically. Cycles are fine.",
             "properties": {
                "type": {"const": "graph"},
                "direction": {"enum": ["down", "right"], "description": "Which way edges point; down if omitted."},
                "nodes": {"type": "array", "items": {"type": "object", "required": ["id"],
                    "properties": {"id": string, "label": {"type": "string", "description": "Shown in the box; the id if omitted."},
                                   "tone": tone}}},
                "edges": {"type": "array", "items": {"type": "object", "required": ["from", "to"],
                    "properties": {"from": string, "to": string, "label": string}}}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "tree"},
                "items": {"type": "array", "items": {"$ref": "#/$defs/tree_item"}}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "steps"},
                "items": {"type": "array", "items": {"type": "object", "required": ["title"],
                    "properties": {"title": string, "text": string,
                        "status": {"enum": ["done", "active", "pending", "failed"]}}}}}},
            {"type": "object", "required": ["type", "before", "after"], "properties": {
                "type": {"const": "diff"}, "path": string,
                "language": {"type": "string", "description": "For highlighting, e.g. rust; from the path if omitted."},
                "before": string, "after": string}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "checklist"},
                "items": {"type": "array", "items": {"type": "object", "required": ["text", "done"],
                    "properties": {"text": string, "done": {"type": "boolean"}}}}}},
            {"type": "object", "required": ["type", "label", "value"], "properties": {
                "type": {"const": "progress"}, "label": string,
                "value": {"type": "number", "minimum": 0, "maximum": 1}}},
            {"type": "object", "required": ["type", "text"], "properties": {
                "type": {"const": "code"}, "language": string, "text": string}},
            {"type": "object", "required": ["type", "tone", "text"], "properties": {
                "type": {"const": "callout"}, "tone": tone, "text": string}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "tags"},
                "items": {"type": "array", "items": {"type": "object", "required": ["label"],
                    "properties": {"label": string, "tone": tone}}}}},
            {"type": "object", "required": ["type", "items"], "properties": {
                "type": {"const": "key_value"},
                "items": {"type": "array", "items": {"type": "object", "required": ["label", "value"],
                    "properties": {"label": string, "value": {}}}}}},
            {"type": "object", "required": ["type", "buttons"], "properties": {
                "type": {"const": "actions"},
                "buttons": {"type": "array", "items": {"type": "object", "required": ["label", "message"],
                    "properties": {"label": string, "message": string}}}}},
            {"type": "object", "required": ["type", "columns"], "properties": {
                "type": {"const": "columns"},
                "columns": {"type": "array", "items": blocks,
                            "description": "Each column is a list of blocks, shown side by side."}}},
            {"type": "object", "required": ["type", "tabs"], "properties": {
                "type": {"const": "tabs"},
                "tabs": {"type": "array", "items": {"type": "object", "required": ["label", "blocks"],
                    "properties": {"label": string, "blocks": blocks}}}}},
            {"type": "object", "required": ["type", "sections"], "properties": {
                "type": {"const": "accordion"},
                "sections": {"type": "array", "items": {"type": "object", "required": ["title", "blocks"],
                    "properties": {"title": string, "blocks": blocks,
                        "open": {"type": "boolean", "description": "Open when first shown."}}}}}}
        ]
    });
    let tree_item = json!({"type": "object", "required": ["label"], "properties": {
        "label": string,
        "hint": {"type": "string", "description": "Dimmed text after the label."},
        "children": {"type": "array", "items": {"$ref": "#/$defs/tree_item"}}}});
    json!({
        "type": "object",
        "required": ["id", "title", "blocks"],
        "properties": {
            "id": {"type": "string", "description": "Stable id; showing the same id again replaces the view."},
            "title": string,
            "blocks": blocks
        },
        "$defs": {"block": block, "tree_item": tree_item}
    })
}

#[cfg(test)]
mod tests {
    use super::{Block, Direction, StepStatus, Tone, View, check, count, input_schema};
    use serde_json::json;

    fn one(block: serde_json::Value) -> Result<View, String> {
        View::parse(&json!({"id": "v", "title": "t", "blocks": [block]}))
    }

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
                {"type": "columns", "columns": [[{"type": "markdown", "text": "l"}], []]},
                {"type": "line_chart", "x": ["mon", "tue"], "series": [{"name": "p95", "values": [1, 2]}]},
                {"type": "area_chart", "x": ["mon"], "series": [{"name": "a", "values": [1]}, {"name": "b", "values": [2]}]},
                {"type": "pie_chart", "donut": true, "slices": [{"label": "a", "value": 3}]},
                {"type": "radar_chart", "axes": ["x", "y", "z"], "series": [{"name": "a", "values": [1, 2, 3]}]},
                {"type": "sankey", "links": [{"from": "in", "to": "out", "value": 5}]},
                {"type": "graph", "direction": "right", "nodes": [{"id": "a"}, {"id": "b", "label": "B", "tone": "success"}],
                 "edges": [{"from": "a", "to": "b", "label": "calls"}]},
                {"type": "tree", "items": [{"label": "src", "children": [{"label": "main.rs", "hint": "169 lines"}]}]},
                {"type": "steps", "items": [{"title": "build", "status": "done"}, {"title": "ship"}]},
                {"type": "diff", "path": "a.rs", "before": "a\n", "after": "b\n"},
                {"type": "tags", "items": [{"label": "rust", "tone": "info"}, {"label": "plain"}]},
                {"type": "key_value", "items": [{"label": "edition", "value": 2024}]},
                {"type": "tabs", "tabs": [{"label": "One", "blocks": [{"type": "markdown", "text": "1"}]}]},
                {"type": "accordion", "sections": [{"title": "More", "open": true, "blocks": []}]}
            ]
        }))
        .unwrap();
        assert_eq!(view.blocks.len(), 23);
        assert_eq!(
            view.blocks[7],
            Block::Callout {
                tone: Tone::Warning,
                text: "careful".into()
            }
        );
        assert!(matches!(&view.blocks[9], Block::Columns { columns } if columns.len() == 2));
        assert!(matches!(
            &view.blocks[15],
            Block::Graph { direction: Direction::Right, nodes, edges }
                if nodes[1].text() == "B" && nodes[0].text() == "a" && edges.len() == 1
        ));
        assert!(matches!(
            &view.blocks[17],
            Block::Steps { items } if items[0].status == StepStatus::Done && items[1].status == StepStatus::Pending
        ));
        assert!(matches!(&view.blocks[22], Block::Accordion { sections } if sections[0].open));
        // Nested blocks count once each: 23 plus columns' 1 and tabs' 1.
        assert_eq!(count(&view.blocks), 25);
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
            (json!({"type": "pie"}), "pie"),
            (
                json!({"type": "table", "columns": ["a", "b"], "rows": [["only one"]]}),
                "1 cells for 2 columns",
            ),
            (
                json!({"type": "progress", "label": "x", "value": 40}),
                "outside 0 to 1",
            ),
            (
                json!({"type": "columns", "columns": [[{"type": "progress", "label": "x", "value": -1}]]}),
                "outside 0 to 1",
            ),
            (
                json!({"type": "line_chart", "x": ["a", "b"], "series": [{"name": "s", "values": [1]}]}),
                "series \"s\" has 1 values for 2 `x` labels",
            ),
            (
                json!({"type": "area_chart", "x": [], "series": []}),
                "at least one `x` label",
            ),
            (
                json!({"type": "pie_chart", "slices": [{"label": "a", "value": -2}]}),
                "slices take 0 or more",
            ),
            (
                json!({"type": "pie_chart", "slices": [{"label": "a", "value": 0}]}),
                "nothing to draw",
            ),
            (
                json!({"type": "radar_chart", "axes": ["a", "b"], "series": []}),
                "at least 3 axes",
            ),
            (
                json!({"type": "radar_chart", "axes": ["a", "b", "c"], "series": [{"name": "s", "values": [1, 2]}]}),
                "2 values for 3 axes",
            ),
            (
                json!({"type": "sankey", "links": [{"from": "a", "to": "b", "value": 1}, {"from": "b", "to": "a", "value": 1}]}),
                "loop back",
            ),
            (
                json!({"type": "sankey", "links": [{"from": "a", "to": "b", "value": 0}]}),
                "more than 0",
            ),
            (
                json!({"type": "graph", "nodes": [{"id": "a"}, {"id": "a"}], "edges": []}),
                "\"a\" is used twice",
            ),
            (
                json!({"type": "graph", "nodes": [{"id": "a"}], "edges": [{"from": "a", "to": "nowhere"}]}),
                "\"nowhere\", which isn't a node id",
            ),
            (
                json!({"type": "graph", "nodes": [], "edges": []}),
                "at least one node",
            ),
            (
                json!({"type": "tabs", "tabs": [{"label": "x", "blocks": [{"type": "progress", "label": "x", "value": 3}]}]}),
                "outside 0 to 1",
            ),
            (
                json!({"type": "accordion", "sections": [{"title": "x", "blocks": [{"type": "tags", "items": [{"label": "a", "tone": "pink"}]}]}]}),
                "pink",
            ),
            (
                json!({"type": "steps", "items": [{"title": "x", "status": "maybe"}]}),
                "maybe",
            ),
        ];
        for (block, expected) in cases {
            let err = one(block.clone()).unwrap_err();
            assert!(err.contains(expected), "{block}: {err}");
        }
        for (arguments, expected) in [
            (json!({"id": " ", "title": "t", "blocks": []}), "`id`"),
            (json!({"id": "v", "blocks": []}), "title"),
            (
                json!({"id": "v", "title": "t", "blocks": [], "colour": "red"}),
                "colour",
            ),
        ] {
            let err = View::parse(&arguments).unwrap_err();
            assert!(err.contains(expected), "{arguments}: {err}");
        }
    }

    /// A graph may loop: state machines do. Only a sankey must not.
    #[test]
    fn graphs_may_have_cycles() {
        let view = one(json!({"type": "graph",
            "nodes": [{"id": "idle"}, {"id": "running"}],
            "edges": [{"from": "idle", "to": "running"}, {"from": "running", "to": "idle"},
                      {"from": "idle", "to": "idle", "label": "tick"}]}));
        assert!(view.is_ok(), "{view:?}");
    }

    #[test]
    fn the_model_hears_what_was_shown_or_what_to_fix() {
        assert_eq!(
            check(&json!({"id": "v", "title": "Plan", "blocks": []})),
            Ok("Shown in the side panel as \"Plan\".".into())
        );
        let err = check(&json!({"id": "v", "title": "t", "blocks": [{"type": "graph", "nodes": [], "edges": []}]}))
            .unwrap_err();
        assert!(
            err.starts_with("Not shown: a graph needs at least one node."),
            "{err}"
        );
        assert!(err.ends_with("call show_view again."), "{err}");
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
        assert_eq!(offered.len(), 23);
        for kind in &offered {
            // Each offered type gets past the type check: any error is
            // about its fields, not an unknown variant.
            let err = one(json!({"type": kind})).unwrap_err();
            assert!(!err.contains("unknown variant"), "{kind}: {err}");
        }
        let err = one(json!({"type": "nope"})).unwrap_err();
        assert!(err.contains("unknown variant"), "{err}");
        // Every $ref points at a definition that exists.
        let text = schema.to_string();
        for reference in text.split("\"$ref\":\"#/$defs/").skip(1) {
            let name = reference.split('"').next().unwrap();
            assert!(schema["$defs"][name].is_object(), "{name}");
        }
    }
}
