//! The right-hand panel. It opens on its own when the session produces
//! something worth showing: the diff of the chat's folder, and views the
//! agent draws with its `show_view` tool, one tab each.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::accordion::Accordion;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::chart::{AreaChart, PieChart, RadarChart, SankeyChart};
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::plot::shape::SankeyLink;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tab::{Tab as PaneTab, TabBar};
use gpui_kit::component::tag::Tag as TagChip;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::graph;
use crate::view::{self, Block, Direction, Series, StepStatus, Tone, TreeItem, View};

/// Which tab is showing: the changes, or a view by id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Changes,
    View(String),
}

/// A button in a view was clicked: its message goes to the agent.
pub struct Reply(pub String);

impl EventEmitter<Reply> for RightPanel {}

pub struct RightPanel {
    diff: Entity<DiffState>,
    file_count: usize,
    views: Vec<Shown>,
    tab: Tab,
    /// The pane picked in each tabs block, by view id and block number.
    panes: HashMap<(String, usize), usize>,
    /// The open sections of each accordion the user has toggled, by view id
    /// and block number; the others show their `open` defaults.
    sections: HashMap<(String, usize), Vec<usize>>,
    /// Where each action button was last drawn, by label: automated runs
    /// click there, through the window's own hit testing.
    action_bounds: ActionBounds,
    view_scroll: ScrollHandle,
}

#[cfg(feature = "automation")]
type ActionBounds = Rc<std::cell::RefCell<Vec<(String, Bounds<Pixels>)>>>;
#[cfg(not(feature = "automation"))]
type ActionBounds = ();

/// A view with the state its blocks render from: markdown and code texts
/// and diffs, by block number.
struct Shown {
    view: View,
    texts: HashMap<usize, Entity<TextViewState>>,
    diffs: HashMap<usize, Entity<DiffState>>,
}

impl RightPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            diff: cx.new(|cx| DiffState::new([], cx)),
            file_count: 0,
            views: Vec::new(),
            tab: Tab::Changes,
            panes: HashMap::new(),
            sections: HashMap::new(),
            action_bounds: Default::default(),
            view_scroll: ScrollHandle::new(),
        }
    }

    /// For automated runs: scrolls the view in front `y` pixels down from
    /// its top.
    #[cfg(feature = "automation")]
    pub fn scroll_view(&mut self, y: f32, cx: &mut Context<Self>) {
        self.view_scroll.set_offset(point(px(0.), px(-y)));
        cx.notify();
    }

    pub fn is_empty(&self) -> bool {
        self.file_count == 0 && self.views.is_empty()
    }

    /// What's shown, for automated checks: the tabs, the one in front, and
    /// the views' blocks by kind.
    #[cfg(feature = "automation")]
    pub fn snapshot(&self) -> serde_json::Value {
        fn kinds(blocks: &[Block]) -> Vec<serde_json::Value> {
            blocks
                .iter()
                .map(|block| match block {
                    Block::Columns { columns } => serde_json::json!({
                        "columns": columns.iter().map(|c| kinds(c)).collect::<Vec<_>>()
                    }),
                    other => serde_json::json!(
                        format!("{other:?}")
                            .split([' ', '{', '('])
                            .next()
                            .unwrap_or_default()
                    ),
                })
                .collect()
        }
        // The tabs and accordions of the view in front, by block number.
        let mut controls = Vec::new();
        if let Some(shown) = self.front() {
            let id = &shown.view.id;
            number_blocks(&shown.view.blocks, &mut 0, &mut |n, block| {
                match block {
                Block::Tabs { tabs } => controls.push(serde_json::json!({
                    "block": n, "kind": "tabs",
                    "labels": tabs.iter().map(|pane| pane.label.clone()).collect::<Vec<_>>(),
                    "picked": self.panes.get(&(id.clone(), n)).copied().unwrap_or(0),
                })),
                Block::Accordion { sections } => controls.push(serde_json::json!({
                    "block": n, "kind": "accordion",
                    "labels": sections.iter().map(|section| section.title.clone()).collect::<Vec<_>>(),
                    "open": self.open_sections(id, n, sections),
                })),
                _ => {}
            }
            });
        }
        serde_json::json!({
            "controls": controls,
            "tab": match &self.tab { Tab::Changes => "changes".to_string(), Tab::View(id) => id.clone() },
            "files": self.file_count,
            "views": self.views.iter().map(|shown| serde_json::json!({
                "id": shown.view.id,
                "title": shown.view.title,
                "blocks": kinds(&shown.view.blocks),
            })).collect::<Vec<_>>(),
            "actions": self.action_bounds.borrow().iter().map(|(label, bounds)| serde_json::json!({
                "label": label,
                "center": [f32::from(bounds.center().x), f32::from(bounds.center().y)],
            })).collect::<Vec<_>>(),
        })
    }

    /// Takes files already parsed off the main thread.
    pub fn show_files(&mut self, files: Vec<DiffFile>, cx: &mut Context<Self>) {
        self.file_count = files.len();
        self.diff.update(cx, |state, cx| state.set_files(files, cx));
        self.keep_tab_valid();
        cx.notify();
    }

    /// The chat's views; `focus` brings that one to the front. Views that
    /// didn't change keep their rendered state.
    pub fn show_views(&mut self, views: &[View], focus: Option<&str>, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.views);
        self.views = views
            .iter()
            .map(
                |view| match old.iter().position(|shown| shown.view == *view) {
                    Some(ix) => old.swap_remove(ix),
                    None => {
                        // A changed view starts over: its blocks may have moved.
                        self.panes.retain(|(id, _), _| *id != view.id);
                        self.sections.retain(|(id, _), _| *id != view.id);
                        shown(view, cx)
                    }
                },
            )
            .collect();
        if let Some(id) = focus {
            self.tab = Tab::View(id.to_string());
        }
        self.keep_tab_valid();
        cx.notify();
    }

    #[cfg(feature = "automation")]
    fn front(&self) -> Option<&Shown> {
        match &self.tab {
            Tab::View(id) => self.views.iter().find(|shown| shown.view.id == *id),
            Tab::Changes => None,
        }
    }

    /// For automated runs: picks a pane of a tabs block in the view in
    /// front, as a click on its tab does.
    #[cfg(feature = "automation")]
    pub fn pick_pane(&mut self, block: usize, index: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.front().map(|shown| shown.view.id.clone()) {
            self.panes.insert((id, block), index);
            cx.notify();
        }
    }

    /// For automated runs: opens or closes a section of an accordion in
    /// the view in front, as a click on its title does.
    #[cfg(feature = "automation")]
    pub fn toggle_section(&mut self, block: usize, index: usize, cx: &mut Context<Self>) {
        let Some(shown) = self.front() else { return };
        let id = shown.view.id.clone();
        let mut sections = None;
        number_blocks(&shown.view.blocks, &mut 0, &mut |n, found| {
            if let (true, Block::Accordion { sections: all }) = (n == block, found) {
                sections = Some(all.clone());
            }
        });
        let Some(sections) = sections else { return };
        let mut open = self.open_sections(&id, block, &sections);
        match open.iter().position(|ix| *ix == index) {
            Some(at) => {
                open.remove(at);
            }
            None => open.push(index),
        }
        self.sections.insert((id, block), open);
        cx.notify();
    }

    /// The open sections of an accordion: as the user left them, or as the
    /// view first showed them.
    fn open_sections(&self, view: &str, block: usize, sections: &[view::Section]) -> Vec<usize> {
        self.sections
            .get(&(view.to_string(), block))
            .cloned()
            .unwrap_or_else(|| {
                sections
                    .iter()
                    .enumerate()
                    .filter(|(_, section)| section.open)
                    .map(|(ix, _)| ix)
                    .collect()
            })
    }

    pub fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.keep_tab_valid();
        cx.notify();
    }

    /// A tab whose content is gone gives way to one that's there.
    fn keep_tab_valid(&mut self) {
        let valid = match &self.tab {
            Tab::Changes => self.file_count > 0,
            Tab::View(id) => self.views.iter().any(|shown| shown.view.id == *id),
        };
        if !valid {
            self.tab = match self.views.last() {
                Some(shown) if self.file_count == 0 => Tab::View(shown.view.id.clone()),
                _ => Tab::Changes,
            };
        }
    }
}

/// Numbers blocks depth first, every nested block included, in the order
/// [`blocks`] lays them out.
fn number_blocks(list: &[Block], n: &mut usize, visit: &mut impl FnMut(usize, &Block)) {
    for block in list {
        *n += 1;
        visit(*n, block);
        for children in block.children() {
            number_blocks(children, n, visit);
        }
    }
}

fn shown(view: &View, cx: &mut App) -> Shown {
    let mut texts = HashMap::new();
    let mut diffs = HashMap::new();
    number_blocks(&view.blocks, &mut 0, &mut |n, block| match block {
        Block::Markdown { text } => {
            texts.insert(n, cx.new(|cx| TextViewState::markdown(text, cx)));
        }
        Block::Code { language, text } => {
            let fenced = format!("```{}\n{text}\n```", language.as_deref().unwrap_or(""));
            texts.insert(n, cx.new(|cx| TextViewState::markdown(&fenced, cx)));
        }
        Block::Diff {
            path,
            language,
            before,
            after,
        } => {
            let files = diff_files(path.as_deref(), language.as_deref(), before, after);
            diffs.insert(n, cx.new(|cx| DiffState::new(files, cx)));
        }
        _ => {}
    });
    Shown {
        view: view.clone(),
        texts,
        diffs,
    }
}

/// One file's change as the diff view reads it.
fn diff_files(
    path: Option<&str>,
    language: Option<&str>,
    before: &str,
    after: &str,
) -> Vec<DiffFile> {
    let name = path.unwrap_or("file");
    let patch = similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string();
    let files = DiffFile::parse(&patch).unwrap_or_default();
    match language {
        Some(language) => files
            .into_iter()
            .map(|file| file.with_language(language.to_string()))
            .collect(),
        None => files,
    }
}

impl Render for RightPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "automation")]
        self.action_bounds.borrow_mut().clear();
        let theme = cx.theme().clone();
        let tab = |id: SharedString,
                   label: String,
                   selected: bool,
                   target: Tab,
                   cx: &mut Context<Self>| {
            div()
                .id(id)
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .text_sm()
                .flex_shrink_0()
                .max_w(px(220.))
                .truncate()
                .text_color(if selected {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(selected, |this| this.bg(theme.secondary))
                .hover(|this| this.bg(theme.secondary))
                .on_click(cx.listener(move |this, _, _, cx| this.select(target.clone(), cx)))
                .child(label)
        };
        let mut tabs = h_flex()
            .id("panel-tabs")
            .gap_1()
            .overflow_x_scroll()
            .min_w_0();
        if self.file_count > 0 {
            let label = format!(
                "{} changed file{}",
                self.file_count,
                if self.file_count == 1 { "" } else { "s" }
            );
            tabs = tabs.child(tab(
                "tab-changes".into(),
                label,
                self.tab == Tab::Changes,
                Tab::Changes,
                cx,
            ));
        }
        for shown in &self.views {
            let target = Tab::View(shown.view.id.clone());
            let selected = self.tab == target;
            tabs = tabs.child(tab(
                format!("tab-{}", shown.view.id).into(),
                shown.view.title.clone(),
                selected,
                target,
                cx,
            ));
        }

        let body = match &self.tab {
            Tab::View(id) => match self.views.iter().find(|shown| shown.view.id == *id) {
                Some(shown) => {
                    let mut draw = Draw {
                        shown,
                        n: 0,
                        actions: &self.action_bounds,
                        panes: &self.panes,
                        open: self,
                    };
                    div()
                        .id("view")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.view_scroll)
                        .p_4()
                        .child(blocks(&shown.view.blocks, &mut draw, cx))
                        .into_any_element()
                }
                None => div().into_any_element(),
            },
            Tab::Changes => div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(Diff::new(&self.diff).size_full())
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h(px(44.))
                    .pl_3()
                    // Clears the panel toggle in the window's corner.
                    .pr(px(48.))
                    .items_center()
                    .child(tabs),
            )
            .child(body)
    }
}

/// What laying out one view needs beyond its blocks.
struct Draw<'a> {
    shown: &'a Shown,
    /// The number of the block last laid out, as [`number_blocks`] counts.
    n: usize,
    #[cfg_attr(not(feature = "automation"), allow(dead_code))]
    actions: &'a ActionBounds,
    panes: &'a HashMap<(String, usize), usize>,
    /// The panel, for which accordion sections are open.
    open: &'a RightPanel,
}

impl Draw<'_> {
    /// Passes over blocks that aren't laid out (a pane not picked, a
    /// section folded), so the ones after keep their numbers.
    fn skip(&mut self, list: &[Block]) {
        self.n += view::count(list);
    }

    fn view_id(&self) -> String {
        self.shown.view.id.clone()
    }
}

/// Lays out blocks top to bottom.
fn blocks(list: &[Block], draw: &mut Draw, cx: &mut Context<RightPanel>) -> Div {
    let mut column = v_flex().gap_4().w_full().min_w_0();
    for block in list {
        column = column.child(block_element(block, draw, cx));
    }
    column
}

/// A block's title line, dimmed above what it heads.
fn heading(title: &Option<String>, theme: &Theme) -> Option<Div> {
    title.clone().map(|title| {
        div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(title)
    })
}

/// Distinct colors for chart series and slices, in order.
fn palette(theme: &Theme) -> [Hsla; 8] {
    [
        theme.blue,
        theme.green,
        theme.yellow,
        theme.magenta,
        theme.cyan,
        theme.red,
        theme.blue_light,
        theme.green_light,
    ]
}

fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Info => theme.info,
        Tone::Success => theme.success,
        Tone::Warning => theme.warning,
        Tone::Error => theme.danger,
    }
}

/// A row of color keys: one swatch and name per series or slice.
fn legend(entries: Vec<(Hsla, String)>, theme: &Theme) -> Div {
    h_flex()
        .flex_wrap()
        .gap_x_3()
        .gap_y_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .children(entries.into_iter().map(|(color, label)| {
            h_flex()
                .gap_1p5()
                .child(div().size(px(8.)).rounded(px(2.)).bg(color))
                .child(label)
        }))
}

fn block_element(block: &Block, draw: &mut Draw, cx: &mut Context<RightPanel>) -> AnyElement {
    let theme = cx.theme().clone();
    draw.n += 1;
    let id = draw.n;
    match block {
        Block::Markdown { .. } | Block::Code { .. } => match draw.shown.texts.get(&id) {
            Some(text) => div()
                .w_full()
                .child(crate::chat::markdown(text))
                .into_any_element(),
            None => div().into_any_element(),
        },
        Block::Table { columns, rows } => {
            // Columns keep a readable width; a wide table scrolls sideways
            // rather than breaking words.
            let cell = |text: String, header: bool| {
                div()
                    .flex_1()
                    .min_w(px(96.))
                    .px_2()
                    .py_1p5()
                    .text_sm()
                    .when(header, |this| this.text_color(theme.muted_foreground))
                    .child(text)
            };
            let mut table = v_flex()
                .w_full()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(
                    h_flex()
                        .bg(theme.secondary)
                        .children(columns.iter().map(|c| cell(c.clone(), true))),
                );
            for row in rows {
                table = table.child(
                    h_flex()
                        .border_t_1()
                        .border_color(theme.border)
                        .children(row.iter().map(|value| cell(view::display(value), false))),
                );
            }
            div()
                .id(("table", id))
                .w_full()
                .overflow_x_scroll()
                .child(table.min_w(px(96. * columns.len() as f32)))
                .into_any_element()
        }
        Block::Stats { items } => h_flex()
            .flex_wrap()
            .gap_3()
            .children(items.iter().map(|stat| {
                v_flex()
                    .min_w(px(120.))
                    .flex_1()
                    .p_3()
                    .gap_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(stat.label.clone()),
                    )
                    .child(div().text_xl().child(view::display(&stat.value)))
                    .when_some(stat.hint.clone(), |this, hint| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        )
                    })
            }))
            .into_any_element(),
        Block::BarChart { title, unit, bars } => {
            let max = bars.iter().map(|bar| bar.value.abs()).fold(0., f64::max);
            let unit = unit.clone().unwrap_or_default();
            v_flex()
                .w_full()
                .gap_1p5()
                .children(heading(title, &theme))
                .children(bars.iter().map(|bar| {
                    let share = if max > 0. {
                        (bar.value.abs() / max) as f32
                    } else {
                        0.
                    };
                    h_flex()
                        .w_full()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .w(px(110.))
                                .flex_shrink_0()
                                .truncate()
                                .child(bar.label.clone()),
                        )
                        .child(
                            div().flex_1().h(px(14.)).child(
                                div()
                                    .h_full()
                                    .w(relative(share))
                                    .rounded(px(3.))
                                    .bg(theme.primary),
                            ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(with_unit(bar.value, &unit)),
                        )
                }))
                .into_any_element()
        }
        Block::LineChart {
            title,
            unit,
            x,
            series,
        } => xy_chart(id, title, unit, x, series, false, &theme),
        Block::AreaChart {
            title,
            unit,
            x,
            series,
        } => xy_chart(id, title, unit, x, series, true, &theme),
        Block::PieChart {
            title,
            unit,
            donut,
            slices,
        } => {
            let colors = palette(&theme);
            let total: f64 = slices.iter().map(|slice| slice.value).sum();
            let unit = unit.clone().unwrap_or_default();
            let data: Vec<(usize, f32)> = slices
                .iter()
                .enumerate()
                .map(|(ix, slice)| (ix, slice.value as f32))
                .collect();
            let names: Rc<Vec<String>> =
                Rc::new(slices.iter().map(|slice| slice.label.clone()).collect());
            let tooltip_unit = unit.clone();
            let mut chart = PieChart::new(data)
                .id(("pie", id))
                .value(|(_, value)| *value)
                .color(move |(ix, _)| colors[ix % colors.len()])
                .outer_radius(90.)
                .pad_angle(0.01)
                .tooltip_name(move |(ix, _)| names[*ix].clone().into())
                .tooltip_value(move |_, value, _| with_unit(value as f64, &tooltip_unit).into());
            if *donut {
                chart = chart.inner_radius(54.);
            }
            let entries = slices
                .iter()
                .enumerate()
                .map(|(ix, slice)| {
                    let share = if total > 0. {
                        slice.value / total * 100.
                    } else {
                        0.
                    };
                    (
                        colors[ix % colors.len()],
                        format!(
                            "{} {} ({share:.0}%)",
                            slice.label,
                            with_unit(slice.value, &unit)
                        ),
                    )
                })
                .collect();
            v_flex()
                .w_full()
                .gap_2()
                .children(heading(title, &theme))
                .child(div().w_full().h(px(200.)).child(chart))
                .child(legend(entries, &theme))
                .into_any_element()
        }
        Block::RadarChart {
            title,
            axes,
            series,
            max,
        } => {
            let colors = palette(&theme);
            let axes: Rc<Vec<String>> = Rc::new(axes.clone());
            let labels = axes.clone();
            let mut chart = RadarChart::new(0..axes.len())
                .id(("radar", id))
                .label(move |ix| SharedString::from(labels[*ix].clone()))
                .label_color(theme.muted_foreground)
                .outer_radius(96.)
                .dot();
            let top = series
                .iter()
                .flat_map(|one| one.values.iter().copied())
                .fold(0., f64::max);
            chart = chart.max_value(max.unwrap_or(top).max(f64::MIN_POSITIVE));
            for (ix, one) in series.iter().enumerate() {
                let values = Rc::new(one.values.clone());
                let color = colors[ix % colors.len()];
                chart = chart
                    .value(move |axis| values[*axis])
                    .stroke(color)
                    .fill(color.opacity(0.18))
                    .name(one.name.clone());
            }
            let entries = series
                .iter()
                .enumerate()
                .map(|(ix, one)| (colors[ix % colors.len()], one.name.clone()))
                .collect();
            v_flex()
                .w_full()
                .gap_2()
                .children(heading(title, &theme))
                .child(div().w_full().h(px(260.)).child(chart))
                .child(legend(entries, &theme))
                .into_any_element()
        }
        Block::Sankey { title, unit, links } => {
            let colors = palette(&theme);
            let mut names: Vec<String> = Vec::new();
            let index = |name: &str, names: &mut Vec<String>| {
                names
                    .iter()
                    .position(|known| known == name)
                    .unwrap_or_else(|| {
                        names.push(name.to_string());
                        names.len() - 1
                    })
            };
            let flows: Vec<SankeyLink> = links
                .iter()
                .map(|link| {
                    let from = index(&link.from, &mut names);
                    let to = index(&link.to, &mut names);
                    SankeyLink::new(from, to, link.value)
                })
                .collect();
            let unit = unit.clone().unwrap_or_default();
            let tooltip_unit = unit.clone();
            let height = (names.len() as f32 * 28.).clamp(200., 420.);
            let chart = SankeyChart::new(names.into_iter().enumerate(), flows)
                .id(("sankey", id))
                .node_color(move |(ix, _)| colors[ix % colors.len()])
                .node_label(|(_, name)| name.clone().into())
                .value_label(move |_, value| with_unit(value, &unit).into())
                .tooltip_name(|(_, name)| name.clone().into())
                .tooltip_value(move |_, value| with_unit(value, &tooltip_unit).into());
            v_flex()
                .w_full()
                .gap_2()
                .children(heading(title, &theme))
                .child(div().w_full().h(px(height)).child(chart))
                .into_any_element()
        }
        Block::Graph {
            direction,
            nodes,
            edges,
        } => graph_element(id, *direction, nodes, edges, &theme),
        Block::Tree { items } => v_flex()
            .w_full()
            .text_sm()
            .child(tree(items, &theme))
            .into_any_element(),
        Block::Steps { items } => {
            let count = items.len();
            v_flex()
                .w_full()
                .children(items.iter().enumerate().map(|(ix, step)| {
                    let (marker, color) = match step.status {
                        StepStatus::Done => (
                            Icon::new(IconName::CircleCheck).small().into_any_element(),
                            theme.success,
                        ),
                        StepStatus::Active => (
                            Icon::new(IconName::LoaderCircle).small().into_any_element(),
                            theme.info,
                        ),
                        StepStatus::Failed => (
                            Icon::new(IconName::CircleX).small().into_any_element(),
                            theme.danger,
                        ),
                        StepStatus::Pending => (
                            div()
                                .size(px(14.))
                                .rounded_full()
                                .border_1()
                                .border_color(theme.muted_foreground)
                                .into_any_element(),
                            theme.muted_foreground,
                        ),
                    };
                    let last = ix + 1 == count;
                    h_flex()
                        .items_start()
                        .gap_3()
                        .child(
                            v_flex()
                                .items_center()
                                .self_stretch()
                                .w(px(16.))
                                .child(div().pt(px(3.)).text_color(color).child(marker))
                                .when(!last, |this| {
                                    this.child(div().flex_1().w(px(1.)).min_h(px(12.)).mt_1().bg(
                                        if step.status == StepStatus::Done {
                                            theme.success.opacity(0.6)
                                        } else {
                                            theme.border
                                        },
                                    ))
                                }),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .pb(if last { px(0.) } else { px(14.) })
                                .child(
                                    div()
                                        .text_sm()
                                        .when(step.status == StepStatus::Pending, |this| {
                                            this.text_color(theme.muted_foreground)
                                        })
                                        .when(step.status == StepStatus::Active, |this| {
                                            this.font_weight(FontWeight::SEMIBOLD)
                                        })
                                        .child(step.title.clone()),
                                )
                                .when_some(step.text.clone(), |this, text| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(text),
                                    )
                                }),
                        )
                }))
                .into_any_element()
        }
        Block::Diff { before, after, .. } => match draw.shown.diffs.get(&id) {
            Some(state) if before != after => {
                // The hunk headers and lines, each a row, under the file's
                // own header row.
                let rows = similar::TextDiff::from_lines(before.as_str(), after.as_str())
                    .unified_diff()
                    .context_radius(3)
                    .to_string()
                    .lines()
                    .filter(|line| !line.starts_with("---") && !line.starts_with("+++"))
                    .count();
                let row = f32::from(theme.font_size) * 1.5;
                let height = (row * 1.9 + rows as f32 * row + 8.).min(480.);
                div()
                    .w_full()
                    .h(px(height))
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(Diff::new(state).size_full())
                    .into_any_element()
            }
            _ => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No changes.")
                .into_any_element(),
        },
        Block::Checklist { items } => v_flex()
            .gap_1p5()
            .children(items.iter().map(|item| {
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(if item.done {
                        Icon::new(IconName::CircleCheck)
                            .small()
                            .text_color(theme.success)
                            .into_any_element()
                    } else {
                        div()
                            .size(px(14.))
                            .mx(px(1.))
                            .rounded_full()
                            .border_1()
                            .border_color(theme.muted_foreground)
                            .into_any_element()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .when(item.done, |this| {
                                this.text_color(theme.muted_foreground).line_through()
                            })
                            .child(item.text.clone()),
                    )
            }))
            .into_any_element(),
        Block::Progress { label, value } => v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .text_sm()
                    .child(label.clone())
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(format!("{:.0}%", value * 100.)),
                    ),
            )
            .child(Progress::new(("progress", id)).value((*value * 100.) as f32))
            .into_any_element(),
        Block::Callout { tone, text } => {
            let icon = match tone {
                Tone::Info => IconName::Info,
                Tone::Success => IconName::CircleCheck,
                Tone::Warning => IconName::TriangleAlert,
                Tone::Error => IconName::CircleX,
            };
            let color = tone_color(*tone, &theme);
            h_flex()
                .items_start()
                .gap_2()
                .p_3()
                .rounded(theme.radius)
                .border_1()
                .border_color(color.alpha(0.5))
                .text_sm()
                .child(Icon::new(icon).small().text_color(color))
                .child(div().min_w_0().flex_1().child(text.clone()))
                .into_any_element()
        }
        Block::Tags { items } => h_flex()
            .flex_wrap()
            .gap_1p5()
            .children(items.iter().map(|tag| {
                let chip = match tag.tone {
                    Some(Tone::Info) => TagChip::info(),
                    Some(Tone::Success) => TagChip::success(),
                    Some(Tone::Warning) => TagChip::warning(),
                    Some(Tone::Error) => TagChip::danger(),
                    None => TagChip::secondary(),
                };
                chip.small().child(tag.label.clone())
            }))
            .into_any_element(),
        Block::KeyValue { items } => v_flex()
            .w_full()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .children(items.iter().enumerate().map(|(ix, pair)| {
                h_flex()
                    .items_start()
                    .text_sm()
                    .when(ix > 0, |this| this.border_t_1().border_color(theme.border))
                    .child(
                        div()
                            .w(relative(0.36))
                            .flex_shrink_0()
                            .px_3()
                            .py_1p5()
                            .bg(theme.secondary)
                            .text_color(theme.muted_foreground)
                            .child(pair.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_3()
                            .py_1p5()
                            .child(view::display(&pair.value)),
                    )
            }))
            .into_any_element(),
        Block::Actions { buttons } => h_flex()
            .flex_wrap()
            .gap_2()
            .children(buttons.iter().enumerate().map(|(ix, action)| {
                let message = action.message.clone();
                let button = Button::new(("action", id * 100 + ix))
                    .label(action.label.clone())
                    .small()
                    .when(ix == 0, |this| this.primary())
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(Reply(message.clone()))));
                #[cfg(feature = "automation")]
                let button = {
                    let bounds = draw.actions.clone();
                    let label = action.label.clone();
                    div().relative().child(button).child(
                        canvas(
                            move |drawn, _, _| bounds.borrow_mut().push((label.clone(), drawn)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                };
                button
            }))
            .into_any_element(),
        Block::Columns { columns } => {
            let mut row = h_flex().items_start().gap_4().w_full();
            for column in columns {
                row = row.child(div().flex_1().min_w_0().child(blocks(column, draw, cx)));
            }
            row.into_any_element()
        }
        Block::Tabs { tabs } => {
            let key = (draw.view_id(), id);
            let picked = draw
                .panes
                .get(&key)
                .copied()
                .unwrap_or(0)
                .min(tabs.len().saturating_sub(1));
            let bar = TabBar::new(("tabs", id))
                .underline()
                .small()
                .selected_index(picked)
                .children(
                    tabs.iter()
                        .map(|pane| PaneTab::new().label(pane.label.clone())),
                )
                .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                    this.panes.insert(key.clone(), *ix);
                    cx.notify();
                }));
            let mut body = None;
            for (ix, pane) in tabs.iter().enumerate() {
                if ix == picked {
                    body = Some(blocks(&pane.blocks, draw, cx));
                } else {
                    draw.skip(&pane.blocks);
                }
            }
            v_flex()
                .w_full()
                .gap_3()
                .child(bar)
                .children(body)
                .into_any_element()
        }
        Block::Accordion { sections } => {
            let key = (draw.view_id(), id);
            let open = draw.open.open_sections(&key.0, id, sections);
            let mut accordion = Accordion::new(("accordion", id)).multiple(true);
            for (ix, section) in sections.iter().enumerate() {
                let is_open = open.contains(&ix);
                let content = if is_open {
                    Some(blocks(&section.blocks, draw, cx))
                } else {
                    draw.skip(&section.blocks);
                    None
                };
                let title = section.title.clone();
                accordion = accordion.item(move |item| {
                    item.title(div().text_sm().child(title))
                        .open(is_open)
                        .children(content)
                });
            }
            let panel = cx.entity().downgrade();
            accordion
                .on_toggle_click(move |open: &[usize], _, cx| {
                    let open = open.to_vec();
                    let key = key.clone();
                    _ = panel.update(cx, |this, cx| {
                        this.sections.insert(key, open);
                        cx.notify();
                    });
                })
                .into_any_element()
        }
    }
}

/// A line or area chart: one series per color, over shared x labels.
fn xy_chart(
    id: usize,
    title: &Option<String>,
    unit: &Option<String>,
    x: &[String],
    series: &[Series],
    filled: bool,
    theme: &Theme,
) -> AnyElement {
    let colors = palette(theme);
    let labels: Rc<Vec<SharedString>> =
        Rc::new(x.iter().map(|label| label.clone().into()).collect());
    let unit = unit.clone().unwrap_or_default();
    let tick_unit = unit.clone();
    let values = series.iter().flat_map(|one| one.values.iter().copied());
    let (low, high) = values.fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let (low, high) = nice_domain(low, high, Y_TICKS);
    let mut chart = AreaChart::new(0..x.len())
        .id(("xy", id))
        .x(move |ix| labels[*ix].clone())
        .y_axis(true)
        .y_domain(low, high)
        // The round domain is the axis: no headroom past its top tick.
        .y_padding(0., 0.)
        .y_tick_count(Y_TICKS + 1)
        .y_tick_format(move |value| with_unit(value, &tick_unit))
        .tooltip_value(move |_, _, value| with_unit(value, &unit).into());
    for (ix, one) in series.iter().enumerate() {
        let values = Rc::new(one.values.clone());
        let color = colors[ix % colors.len()];
        chart = chart
            .y(move |point| values[*point])
            .stroke(color)
            .fill(if filled {
                color.opacity(0.22)
            } else {
                color.opacity(0.)
            })
            .name(one.name.clone());
    }
    let entries: Vec<(Hsla, String)> = series
        .iter()
        .enumerate()
        .map(|(ix, one)| (colors[ix % colors.len()], one.name.clone()))
        .collect();
    v_flex()
        .w_full()
        .gap_2()
        .children(heading(title, theme))
        .child(div().w_full().h(px(200.)).child(chart))
        .when(series.len() > 1, |this| this.child(legend(entries, theme)))
        .into_any_element()
}

/// Steps up a chart's value axis.
const Y_TICKS: usize = 4;

/// A value axis from `low` to `high` widened to round ends, so its `ticks`
/// steps fall on round numbers: 0 to 600 by 150, not 0 to 507.91.
fn nice_domain(low: f64, high: f64, ticks: usize) -> (f64, f64) {
    let span = high - low;
    if span.is_nan() || span <= 0. {
        return (low.min(0.), low.max(0.) + 1.);
    }
    let rough = span / ticks as f64;
    let magnitude = 10f64.powf(rough.log10().floor());
    let step = [1., 1.5, 2., 2.5, 3., 4., 5., 10.]
        .iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= rough)
        .unwrap_or(10. * magnitude);
    let mut start = (low / step).floor() * step;
    // Rounding out both ends can take more steps than asked for: move the
    // start up while the top still clears the data.
    while start + step * (ticks as f64) < high {
        start += step;
    }
    let start = if low >= 0. { start.max(0.) } else { start };
    let mut step = step;
    while start + step * (ticks as f64) < high {
        step *= 2.;
    }
    (start, start + step * ticks as f64)
}

fn tree(items: &[TreeItem], theme: &Theme) -> Div {
    v_flex().children(items.iter().map(|item| {
        let branch = !item.children.is_empty();
        v_flex()
            .child(
                h_flex()
                    .gap_1p5()
                    .py(px(2.))
                    .child(
                        Icon::new(if branch {
                            IconName::FolderOpen
                        } else {
                            IconName::File
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    )
                    .child(div().min_w_0().truncate().child(item.label.clone()))
                    .when_some(item.hint.clone(), |this, hint| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        )
                    }),
            )
            .when(branch, |this| {
                this.child(
                    div()
                        .ml(px(7.))
                        .pl(px(12.))
                        .border_l_1()
                        .border_color(theme.border)
                        .child(tree(&item.children, theme)),
                )
            })
    }))
}

/// Text in graph boxes: the small UI size, in a monospace font, so a
/// label's width is its length.
const GRAPH_TEXT: f32 = 13.;
const GRAPH_LINE: f32 = 18.;
const GRAPH_CHAR: f32 = GRAPH_TEXT * 0.6;
/// The widest a box gets before its label wraps.
const GRAPH_WRAP: usize = 22;

/// A label broken into lines of at most [`GRAPH_WRAP`] characters, at
/// spaces where it can.
fn wrap(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while word.chars().count() > GRAPH_WRAP {
            let head: String = word.chars().take(GRAPH_WRAP).collect();
            word = word.chars().skip(GRAPH_WRAP).collect();
            lines.push(head);
        }
        match lines.last_mut() {
            Some(line)
                if !line.is_empty()
                    && line.chars().count() + 1 + word.chars().count() <= GRAPH_WRAP =>
            {
                line.push(' ');
                line.push_str(&word);
            }
            _ => lines.push(word),
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn graph_element(
    id: usize,
    direction: Direction,
    nodes: &[view::Node],
    edges: &[view::Edge],
    theme: &Theme,
) -> AnyElement {
    let lines: Vec<Vec<String>> = nodes.iter().map(|node| wrap(node.text())).collect();
    let sizes: Vec<(f32, f32)> = lines
        .iter()
        .map(|lines| {
            let chars = lines
                .iter()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0);
            let width = (chars as f32 * GRAPH_CHAR + 24.).max(56.);
            let height = lines.len() as f32 * GRAPH_LINE + 14.;
            (width.ceil(), height)
        })
        .collect();
    let index: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(ix, node)| (node.id.as_str(), ix))
        .collect();
    let pairs: Vec<(usize, usize)> = edges
        .iter()
        .map(|edge| (index[edge.from.as_str()], index[edge.to.as_str()]))
        .collect();
    let labeled = edges.iter().any(|edge| edge.label.is_some());
    let laid = graph::layout(&sizes, &pairs, direction, labeled);

    let stroke = theme.muted_foreground.opacity(0.75);
    let routes = laid.edges.clone();
    let edge_canvas = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let at = |p: graph::Point| point(bounds.origin.x + px(p.x), bounds.origin.y + px(p.y));
            for route in &routes {
                let mut path = PathBuilder::stroke(px(1.25));
                let points = &route.points;
                path.move_to(at(points[0]));
                let mut tip_from = points[0];
                for pair in points.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    if route.self_loop {
                        // Out sideways, round, and back in.
                        let reach = match direction {
                            Direction::Down => graph::Point { x: b.x, y: a.y },
                            Direction::Right => graph::Point { x: a.x, y: b.y },
                        };
                        path.curve_to(at(b), at(reach));
                    } else {
                        // Leave and enter along the main axis.
                        let (c1, c2) = match direction {
                            Direction::Down => {
                                let mid = (a.y + b.y) / 2.;
                                (
                                    graph::Point { x: a.x, y: mid },
                                    graph::Point { x: b.x, y: mid },
                                )
                            }
                            Direction::Right => {
                                let mid = (a.x + b.x) / 2.;
                                (
                                    graph::Point { x: mid, y: a.y },
                                    graph::Point { x: mid, y: b.y },
                                )
                            }
                        };
                        path.cubic_bezier_to(at(b), at(c1), at(c2));
                        tip_from = c2;
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, stroke);
                }
                // The arrowhead, pointing the way the edge arrives.
                let tip = points[points.len() - 1];
                let (dx, dy) = if route.self_loop {
                    match direction {
                        Direction::Down => (-1., 0.),
                        Direction::Right => (0., -1.),
                    }
                } else {
                    let (dx, dy) = (tip.x - tip_from.x, tip.y - tip_from.y);
                    let len = (dx * dx + dy * dy).sqrt().max(0.001);
                    (dx / len, dy / len)
                };
                let (size, half) = (7., 3.5);
                let base = graph::Point {
                    x: tip.x - dx * size,
                    y: tip.y - dy * size,
                };
                let mut head = PathBuilder::fill();
                head.move_to(at(tip));
                head.line_to(at(graph::Point {
                    x: base.x - dy * half,
                    y: base.y + dx * half,
                }));
                head.line_to(at(graph::Point {
                    x: base.x + dy * half,
                    y: base.y - dx * half,
                }));
                head.close();
                if let Ok(head) = head.build() {
                    window.paint_path(head, stroke);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();

    let boxes = nodes.iter().enumerate().map(|(ix, node)| {
        let at = laid.nodes[ix];
        let (width, height) = sizes[ix];
        let (border, fill) = match node.tone {
            Some(tone) => {
                let color = tone_color(tone, theme);
                (color.opacity(0.8), color.opacity(0.14))
            }
            None => (theme.border, theme.background),
        };
        v_flex()
            .absolute()
            .left(px(at.x))
            .top(px(at.y))
            .w(px(width))
            .h(px(height))
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .border_1()
            .border_color(border)
            .bg(theme.sidebar)
            .child(v_flex().absolute().size_full().rounded(px(6.)).bg(fill))
            .text_size(px(GRAPH_TEXT))
            .line_height(px(GRAPH_LINE))
            .children(lines[ix].iter().map(|line| div().child(line.clone())))
    });
    let labels = edges.iter().zip(&laid.edges).filter_map(|(edge, route)| {
        let label = edge.label.clone()?;
        let width = label.chars().count() as f32 * GRAPH_CHAR * (11. / GRAPH_TEXT) + 10.;
        let at = route.label_at;
        let left = if route.self_loop {
            at.x
        } else {
            at.x - width / 2.
        };
        Some(
            div()
                .absolute()
                .left(px(left.max(0.)))
                .top(px(at.y - 8.))
                .h(px(16.))
                .px(px(5.))
                .rounded(px(4.))
                .bg(theme.sidebar)
                .text_size(px(11.))
                .line_height(px(16.))
                .text_color(theme.muted_foreground)
                .child(label),
        )
    });
    div()
        .id(("graph", id))
        .w_full()
        .overflow_x_scroll()
        .child(
            div()
                .relative()
                .flex_shrink_0()
                .mx_auto()
                .w(px(laid.width))
                .h(px(laid.height))
                .child(edge_canvas)
                .children(boxes)
                .children(labels),
        )
        .into_any_element()
}

/// A bar's value as read: thousands grouped, at most two decimals, none
/// that are zero ("499,500", "2.5").
fn number(value: f64) -> String {
    let text = format!("{:.2}", value.abs());
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let fraction = fraction.trim_end_matches('0');
    let mut grouped = String::new();
    for (ix, digit) in whole.chars().enumerate() {
        if ix > 0 && (whole.len() - ix) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let sign = if value < 0. && (whole != "0" || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

/// A unit after a number: words get a space ("12 comparisons"), symbols
/// don't ("12ms", "40%").
fn with_unit(value: f64, unit: &str) -> String {
    if unit.starts_with(char::is_alphabetic) && unit.chars().count() > 2 {
        format!("{} {unit}", number(value))
    } else {
        format!("{}{unit}", number(value))
    }
}

#[cfg(test)]
mod tests {
    use super::{GRAPH_WRAP, nice_domain, number, with_unit, wrap};

    #[test]
    fn chart_axes_end_on_round_numbers() {
        assert_eq!(nice_domain(0., 520., 4), (0., 600.));
        assert_eq!(nice_domain(0., 8_900., 4), (0., 10_000.));
        assert_eq!(nice_domain(0., 100., 4), (0., 100.));
        assert_eq!(nice_domain(0., 7., 4), (0., 8.));
        assert_eq!(nice_domain(-30., 70., 4), (-25., 75.));
        assert_eq!(nice_domain(0., 0., 4), (0., 1.));
        for (low, high) in [(0., 1.), (0., 3.3), (12., 980.), (-1., 1.), (0., 123_456.)] {
            let (a, b) = nice_domain(low, high, 4);
            assert!(a <= low && b >= high, "{low}..{high} -> {a}..{b}");
        }
    }
    use crate::view::{Block, View, count};

    #[test]
    fn numbers_read_as_written_by_hand() {
        assert_eq!(number(499_500.), "499,500");
        assert_eq!(number(1_000.), "1,000");
        assert_eq!(number(999.), "999");
        assert_eq!(number(2.5), "2.5");
        assert_eq!(number(3.0), "3");
        assert_eq!(number(-12_345.678), "-12,345.68");
        assert_eq!(number(0.), "0");
        assert_eq!(number(-0.001), "0");
        assert_eq!(with_unit(8_700., "comparisons"), "8,700 comparisons");
        assert_eq!(with_unit(12., "ms"), "12ms");
        assert_eq!(with_unit(40., "%"), "40%");
    }

    #[test]
    fn graph_labels_wrap_at_spaces_and_split_long_words() {
        assert_eq!(wrap("sense-core"), vec!["sense-core"]);
        assert_eq!(
            wrap("Pure lifecycle state machine with no IO"),
            vec!["Pure lifecycle state", "machine with no IO"]
        );
        let long = "a".repeat(GRAPH_WRAP + 3);
        assert_eq!(wrap(&long), vec!["a".repeat(GRAPH_WRAP), "aaa".into()]);
        assert_eq!(wrap(""), vec![String::new()]);
        for line in wrap("one two three four five six seven eight nine ten") {
            assert!(line.chars().count() <= GRAPH_WRAP, "{line}");
        }
    }

    /// The numbering that keys markdown and diff state counts the same
    /// blocks laying out walks: hidden panes and folded sections included.
    #[test]
    fn block_numbers_match_between_numbering_and_layout() {
        let view = View::parse(&serde_json::json!({"id": "v", "title": "t", "blocks": [
            {"type": "tabs", "tabs": [
                {"label": "a", "blocks": [{"type": "markdown", "text": "1"}, {"type": "markdown", "text": "2"}]},
                {"label": "b", "blocks": [{"type": "columns", "columns": [[{"type": "markdown", "text": "3"}]]}]}
            ]},
            {"type": "accordion", "sections": [{"title": "s", "blocks": [{"type": "markdown", "text": "4"}]}]},
            {"type": "markdown", "text": "5"}
        ]}))
        .unwrap();
        let mut numbered = Vec::new();
        super::number_blocks(&view.blocks, &mut 0, &mut |n, block| {
            if let Block::Markdown { text } = block {
                numbered.push((n, text.clone()));
            }
        });
        assert_eq!(
            numbered,
            vec![
                (2, "1".to_string()),
                (3, "2".to_string()),
                (5, "3".to_string()),
                (7, "4".to_string()),
                (8, "5".to_string())
            ]
        );
        assert_eq!(count(&view.blocks), 8);
    }
}
