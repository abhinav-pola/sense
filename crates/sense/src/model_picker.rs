//! Choosing a model: a search field over OpenRouter's list, fuzzy matched
//! on name and id. Arrow keys move, Enter picks, Escape closes.

use std::sync::Arc;

use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::models::Model;

pub enum PickerEvent {
    Picked(String),
    Dismiss,
}

impl EventEmitter<PickerEvent> for ModelPicker {}

pub struct ModelPicker {
    query: Entity<InputState>,
    models: Arc<Vec<Model>>,
    current: String,
    /// Indexes into `models` that match the query, best first.
    matches: Vec<usize>,
    /// Which of `matches` Enter picks.
    highlighted: usize,
    scroll: ScrollHandle,
    /// Opened, not yet drawn: the search field takes the cursor once it's
    /// on screen to take it.
    focus_pending: bool,
}

impl ModelPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.refilter(cx),
                InputEvent::PressEnter { .. } => this.pick(this.highlighted, cx),
                _ => {}
            },
        )
        .detach();
        Self {
            query,
            models: Default::default(),
            current: String::new(),
            matches: Vec::new(),
            highlighted: 0,
            scroll: ScrollHandle::new(),
            focus_pending: false,
        }
    }

    /// Fresh each time it opens: no query, the current model highlighted,
    /// the cursor in the search field.
    pub fn open(
        &mut self,
        models: Arc<Vec<Model>>,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.models = models;
        self.current = current;
        self.query
            .update(cx, |query, cx| query.set_value("", window, cx));
        self.focus_pending = true;
        self.refilter(cx);
        self.highlighted = self
            .matches
            .iter()
            .position(|&ix| self.models[ix].id == self.current)
            .unwrap_or(0);
        self.scroll.scroll_to_item(self.highlighted);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_string();
        let texts: Vec<String> = self
            .models
            .iter()
            .map(|model| format!("{} {}", model.short_name(), model.id))
            .collect();
        self.matches = crate::fuzzy::rank(&query, texts.iter().map(String::as_str));
        self.highlighted = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    fn pick(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(&ix) = self.matches.get(n) {
            cx.emit(PickerEvent::Picked(self.models[ix].id.clone()));
        }
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() as isize - 1;
        self.highlighted = (self.highlighted as isize + by).clamp(0, last) as usize;
        self.scroll.scroll_to_item(self.highlighted);
        cx.notify();
    }

    /// What's listed, for automated checks: ids in order, and the one Enter
    /// would pick.
    #[cfg(feature = "automation")]
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "matches": self.matches.iter().map(|&ix| self.models[ix].id.clone()).collect::<Vec<_>>(),
            "highlighted": self.matches.get(self.highlighted).map(|&ix| self.models[ix].id.clone()),
        })
    }
}

impl Render for ModelPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_pending) {
            self.query.update(cx, |query, cx| query.focus(window, cx));
        }
        let theme = cx.theme().clone();
        let rows = self.matches.iter().enumerate().map(|(n, &ix)| {
            let model = &self.models[ix];
            let current = model.id == self.current;
            h_flex()
                .id(("model", ix))
                .gap_2()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .when(n == self.highlighted, |this| this.bg(theme.accent))
                .hover(|this| this.bg(theme.accent))
                .on_click(cx.listener(move |this, _, _, cx| this.pick(n, cx)))
                .child(div().w(px(14.)).flex_shrink_0().when(current, |this| {
                    this.child(Icon::new(IconName::Check).xsmall())
                }))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .child(model.short_name().to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(model.id.clone()),
                        ),
                )
        });
        v_flex()
            .w(px(320.))
            .gap_1()
            // Arrow keys move through the list rather than the search text.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.step(-1, cx),
                    "down" => this.step(1, cx),
                    "escape" => cx.emit(PickerEvent::Dismiss),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(TextInput::new(&self.query).small())
            .child(
                div()
                    .id("model-list")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(self.matches.is_empty(), |this| {
                        this.child(
                            div()
                                .p_2()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(if self.models.is_empty() {
                                    "No models yet: refresh them in the account menu"
                                } else {
                                    "No model matches"
                                }),
                        )
                    })
                    .children(rows),
            )
    }
}
