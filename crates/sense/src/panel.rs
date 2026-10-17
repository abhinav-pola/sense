//! The right-hand panel. It opens on its own when the session produces
//! something worth showing: the diff of the chat's folder, and views the
//! agent draws with its `show_view` tool, one tab each.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::text::TextViewState;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::view::{self, Block, Tone, View};

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
    /// Where each action button was last drawn, by label: automated runs
    /// click there, through the window's own hit testing.
    action_bounds: ActionBounds,
}

#[cfg(feature = "automation")]
type ActionBounds = std::rc::Rc<std::cell::RefCell<Vec<(String, Bounds<Pixels>)>>>;
#[cfg(not(feature = "automation"))]
type ActionBounds = ();

/// A view with the text states its markdown and code blocks render from,
/// in the order the blocks are walked.
struct Shown {
    view: View,
    texts: Vec<Entity<TextViewState>>,
}

impl RightPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            diff: cx.new(|cx| DiffState::new([], cx)),
            file_count: 0,
            views: Vec::new(),
            tab: Tab::Changes,
            action_bounds: Default::default(),
        }
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
        serde_json::json!({
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
    /// didn't change keep their rendered text.
    pub fn show_views(&mut self, views: &[View], focus: Option<&str>, cx: &mut Context<Self>) {
        let mut old = std::mem::take(&mut self.views);
        self.views = views
            .iter()
            .map(
                |view| match old.iter().position(|shown| shown.view == *view) {
                    Some(ix) => old.swap_remove(ix),
                    None => Shown {
                        view: view.clone(),
                        texts: texts_of(&view.blocks, cx),
                    },
                },
            )
            .collect();
        if let Some(id) = focus {
            self.tab = Tab::View(id.to_string());
        }
        self.keep_tab_valid();
        cx.notify();
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

/// Text states for the markdown and code blocks, walked depth first.
fn texts_of(blocks: &[Block], cx: &mut App) -> Vec<Entity<TextViewState>> {
    let mut texts = Vec::new();
    walk(blocks, &mut |block| match block {
        Block::Markdown { text } => texts.push(cx.new(|cx| TextViewState::markdown(text, cx))),
        Block::Code { language, text } => {
            let fenced = format!("```{}\n{text}\n```", language.as_deref().unwrap_or(""));
            texts.push(cx.new(|cx| TextViewState::markdown(&fenced, cx)));
        }
        _ => {}
    });
    texts
}

fn walk(blocks: &[Block], visit: &mut impl FnMut(&Block)) {
    for block in blocks {
        visit(block);
        if let Block::Columns { columns } = block {
            for column in columns {
                walk(column, visit);
            }
        }
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
                    let mut texts = shown.texts.iter();
                    div()
                        .id("view")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .child(blocks(
                            &shown.view.blocks,
                            &mut texts,
                            &mut 0,
                            &self.action_bounds,
                            cx,
                        ))
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

/// Lays out blocks top to bottom. `texts` hands out the markdown states in
/// walk order; `n` numbers elements that need ids.
fn blocks<'a>(
    list: &[Block],
    texts: &mut impl Iterator<Item = &'a Entity<TextViewState>>,
    n: &mut usize,
    actions: &ActionBounds,
    cx: &mut Context<RightPanel>,
) -> Div {
    let mut column = v_flex().gap_4().w_full().min_w_0();
    for block in list {
        column = column.child(block_element(block, texts, n, actions, cx));
    }
    column
}

fn block_element<'a>(
    block: &Block,
    texts: &mut impl Iterator<Item = &'a Entity<TextViewState>>,
    n: &mut usize,
    actions: &ActionBounds,
    cx: &mut Context<RightPanel>,
) -> AnyElement {
    let theme = cx.theme().clone();
    *n += 1;
    let id = *n;
    match block {
        Block::Markdown { .. } | Block::Code { .. } => match texts.next() {
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
                .when_some(title.clone(), |this, title| {
                    this.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(title),
                    )
                })
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
            let (icon, color) = match tone {
                Tone::Info => (IconName::Info, theme.info),
                Tone::Success => (IconName::CircleCheck, theme.success),
                Tone::Warning => (IconName::TriangleAlert, theme.warning),
                Tone::Error => (IconName::CircleX, theme.danger),
            };
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
                    let bounds = actions.clone();
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
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(blocks(column, texts, n, actions, cx)),
                );
            }
            row.into_any_element()
        }
    }
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
    let sign = if value < 0. { "-" } else { "" };
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
    use super::{number, with_unit};

    #[test]
    fn numbers_read_as_written_by_hand() {
        assert_eq!(number(499_500.), "499,500");
        assert_eq!(number(1_000.), "1,000");
        assert_eq!(number(999.), "999");
        assert_eq!(number(2.5), "2.5");
        assert_eq!(number(3.0), "3");
        assert_eq!(number(-12_345.678), "-12,345.68");
        assert_eq!(number(0.), "0");
        assert_eq!(with_unit(8_700., "comparisons"), "8,700 comparisons");
        assert_eq!(with_unit(12., "ms"), "12ms");
        assert_eq!(with_unit(40., "%"), "40%");
    }
}
