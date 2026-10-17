//! One conversation: its transcript, composer and the fx session behind it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::clipboard::Clipboard;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sense_agent::PermissionAsk;
use sense_agent::acp::{
    ContentBlock, NoticeSeverity, PermissionOptionKind, SessionUpdate, ToolCallId, ToolCallStatus,
    ToolKind,
};
use sense_core::ChatId;

use crate::snapshot::Snapshot;
use crate::thumb::Thumb;
use gpui_kit::component::diff::DiffFile;

pub enum Entry {
    /// The user's text and any images they attached.
    User(SharedString, Vec<Thumb>),
    /// The markdown view plus its source, which the view does not expose.
    Agent(Entity<TextViewState>, String),
    Thought(Entity<TextViewState>, String),
    Tool {
        id: ToolCallId,
        title: SharedString,
        status: ToolCallStatus,
    },
    Permission {
        title: SharedString,
        options: Vec<(SharedString, bool)>,
        /// Boxed so a pending request doesn't inflate every transcript entry.
        ask: Option<Box<PermissionAsk>>,
    },
    Notice(SharedString),
    /// fx's informational notices (skill catalog, MCP budgets, ...): a
    /// one-line title, details shown when expanded.
    Diagnostic {
        title: SharedString,
        details: Option<SharedString>,
        expanded: bool,
    },
}

/// How much of `held` the next frame reveals: a share of the backlog,
/// extended to the end of the word when that's close. Always a character
/// boundary within `held`, and more than nothing when `held` isn't empty.
fn reveal_cut(held: &str) -> usize {
    let want = (held.len() / REVEAL_SHARE).max(REVEAL_MIN).min(held.len());
    let mut end = (want..=held.len())
        .find(|ix| held.is_char_boundary(*ix))
        .unwrap_or(held.len());
    if let Some((space, ch)) = held[end..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        && space <= 12
    {
        end += space + ch.len_utf8();
    }
    end
}

fn short_duration(took: Duration) -> String {
    match took.as_secs() {
        0 => "under a second".into(),
        secs @ 1..60 => format!("{secs}s"),
        secs => format!("{}m {}s", secs / 60, secs % 60),
    }
}

/// Where a thread is, in the terms of fx's Program Status Protocol
/// (OSC 7501, vercel-labs/fx#1181), read from what ACP reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing has run yet.
    Idle,
    /// Not ready to take a message (a worktree being made).
    Preparing(SharedString),
    Working {
        since: Instant,
    },
    /// Waiting on the user to allow a tool.
    NeedsPermission(SharedString),
    Done {
        at: Instant,
    },
    Stopped {
        at: Instant,
    },
    Failed {
        at: Instant,
        error: SharedString,
    },
}

/// A message written while the agent was working, sent when it's done.
struct Queued {
    text: String,
    attachments: Vec<Attachment>,
}

struct Attachment {
    mime: String,
    bytes: Arc<[u8]>,
    /// The same image, decoded once, at the size it's shown.
    thumb: Thumb,
}

const IMAGE_TYPES: &[(&str, &str, ImageFormat)] = &[
    ("png", "image/png", ImageFormat::Png),
    ("jpg", "image/jpeg", ImageFormat::Jpeg),
    ("jpeg", "image/jpeg", ImageFormat::Jpeg),
    ("gif", "image/gif", ImageFormat::Gif),
    ("webp", "image/webp", ImageFormat::Webp),
];

fn image_format(mime: &str) -> Option<ImageFormat> {
    IMAGE_TYPES
        .iter()
        .find(|(_, m, _)| *m == mime)
        .map(|(_, _, format)| *format)
}

fn mime_for_path(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES
        .iter()
        .find(|(e, _, _)| *e == ext)
        .map(|(_, mime, _)| *mime)
}

/// Image files among `paths`, read.
fn read_images(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)> {
    paths
        .iter()
        .filter_map(|path| Some((mime_for_path(path)?.to_string(), std::fs::read(path).ok()?)))
        .collect()
}

fn images_on_clipboard(item: &ClipboardItem) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in item.entries() {
        match entry {
            ClipboardEntry::Image(image) => {
                let mime = IMAGE_TYPES
                    .iter()
                    .find(|(_, _, format)| *format == image.format())
                    .map(|(_, mime, _)| *mime);
                if let Some(mime) = mime {
                    found.push((mime.to_string(), image.bytes().to_vec()));
                }
            }
            ClipboardEntry::ExternalPaths(paths) => found.extend(read_images(paths.paths())),
            _ => {}
        }
    }
    found
}

/// A history image (base64, as ACP carries it) for display.
fn decode_image(data: &str, mime: &str) -> Option<Thumb> {
    use base64::Engine as _;
    image_format(mime)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    crate::thumb::thumbnail(&bytes)
}

/// Hover group that reveals a message's actions.
const MESSAGE_GROUP: &str = "message";

/// Copies a message's text, visible while the message is hovered. Space is
/// reserved either way so hovering doesn't shift the transcript.
fn copy_button(ix: usize, text: SharedString) -> impl IntoElement {
    div()
        .pt_1()
        .invisible()
        .group_hover(MESSAGE_GROUP, |style| style.visible())
        .child(Clipboard::new(("copy", ix)).value(text))
}

/// Markdown with LaTeX math and a copy button on each code block, for the
/// code alone.
pub fn markdown(state: &Entity<TextViewState>) -> TextView {
    TextView::new(state)
        .markdown_extensions(crate::math::extensions())
        .code_block_actions(|block, _, _| {
            let code = block.code();
            let id = {
                use std::hash::{Hash as _, Hasher as _};
                let mut hasher = std::hash::DefaultHasher::new();
                code.hash(&mut hasher);
                hasher.finish()
            };
            Clipboard::new(("copy-code", id as usize)).value(code)
        })
}

/// How often held stream text is revealed: once a frame at 60 Hz.
const REVEAL_FRAME: Duration = Duration::from_millis(16);
/// Each frame reveals 1/REVEAL_SHARE of the backlog, at least REVEAL_MIN
/// bytes, so the reveal catches up within a few frames of any burst.
const REVEAL_SHARE: usize = 6;
const REVEAL_MIN: usize = 3;
/// Older history is fetched once the view is this close to the top of what's
/// loaded: about a screenful ahead of the reader.
const HISTORY_PREFETCH: Pixels = px(800.);
/// Rows that roughly fill the transcript; fewer leaves room for older ones.
const SCREENFUL_ROWS: usize = 12;
/// New tool rows and notices fade in over this long.
const ROW_FADE: Duration = Duration::from_millis(120);

/// How much of a past session's transcript is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum History {
    /// A past session not opened yet.
    NotLoaded,
    /// A page was asked for: the first (newest), or the one at `cursor`.
    Loading { first: bool, cursor: Option<String> },
    /// Asking for that page failed; it's asked for again when the session
    /// store is next available.
    Failed { first: bool, cursor: Option<String> },
    /// Older turns exist; this cursor fetches the next page.
    More(String),
    /// Everything is shown (or the chat is new).
    Complete,
}

/// A finished turn's working steps, between the user's message and the
/// agent's final answer, shown as one row until expanded.
struct Fold {
    first: usize,
    last: usize,
    steps: usize,
    took: Option<Duration>,
    expanded: bool,
}

pub enum ChatEvent {
    /// The user sent a prompt. The lifecycle decides when it reaches fx.
    Submit(sense_core::Prompt),
    Stop,
    BaselineTaken(Option<String>),
    TitleChanged,
    /// Scrolled near the top of a past session with older turns to fetch.
    NeedOlderHistory,
    /// `ChatView::patch` changed: the files on disk differ from before.
    PatchChanged,
    /// The agent showed (or replaced) the view with this id.
    ViewShown(String),
}

impl EventEmitter<ChatEvent> for ChatView {}

/// The transcript, composer and diff of one chat. Whether it is busy, its
/// session and its folder come from `sense_core::Lifecycle` through
/// [`ChatView::sync`]; the view itself decides nothing about the lifecycle.
pub struct ChatView {
    pub id: ChatId,
    pub title: SharedString,
    pub cwd: PathBuf,
    pub session: Option<String>,
    /// The worktree this thread was made to work in, whether or not it's
    /// ready yet, and its branch.
    pub worktree: Option<(PathBuf, SharedString)>,
    busy: bool,
    baseline_taking: bool,
    /// Stop was pressed during the current turn.
    stopped: bool,
    entries: Vec<Entry>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    /// Streamed text not yet shown: the transcript reveals whole lines.
    held: String,
    held_thought: bool,
    /// A reveal timer is draining `held`.
    revealing: bool,
    /// Updates being applied come from history, not a live turn.
    loading_history: bool,
    /// Images waiting to go with the next message.
    attachments: Vec<Attachment>,
    /// Why sending isn't possible yet, e.g. its worktree is being created.
    pub preparing: Option<SharedString>,
    /// A subagent's transcript: no composer, its diff from its own edits.
    pub read_only: bool,
    edits: std::collections::BTreeMap<PathBuf, (String, String)>,
    edits_changed: bool,
    pub history: History,
    /// When each entry was added, for its fade-in.
    born: Vec<Instant>,
    /// Which fold, if any, each entry belongs to.
    fold_of: Vec<Option<usize>>,
    folds: Vec<Fold>,
    /// Where the turn in progress starts: its user message.
    turn_start: Option<usize>,
    snapshot: Option<Arc<Snapshot>>,
    /// Unified diff of the folder against the baseline.
    pub patch: String,
    /// `patch`, parsed for the diff view.
    pub patch_files: Vec<DiffFile>,
    /// Views the agent drew for the side panel, oldest first.
    pub views: Vec<crate::view::View>,
    /// The model this chat's prompts ask for.
    pub model: String,
    queued: Option<Queued>,
    /// When the running turn started.
    working_since: Option<Instant>,
    /// How the last turn ended, if one has.
    ended: Option<Status>,
    /// The running turn failed: set by the workspace from the lifecycle's
    /// notice, read when the turn ends.
    pub turn_failed: Option<SharedString>,
    /// A stopped turn's queued message goes back to the composer on the next
    /// render (which has the window to put it there).
    unqueue_pending: bool,
    /// Attached images still being decoded: sending waits for them.
    decoding: usize,
    picker: Entity<crate::model_picker::ModelPicker>,
    picker_open: bool,
    /// When a click outside last closed the picker.
    picker_closed: Option<Instant>,
    /// Picked in the composer, rather than the default.
    model_picked: bool,
    /// The models to pick from, shared by every chat.
    pub models: Arc<Vec<crate::models::Model>>,
    /// While a history page goes in: the views already known from newer
    /// pages (or from this run).
    views_from_newer_pages: std::collections::HashSet<String>,
    /// A file-changing tool ran while the baseline was still being taken.
    edited_before_baseline: bool,
    diff_running: bool,
    diff_stale: bool,
}

impl ChatView {
    pub fn new(id: ChatId, cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let picker = cx.new(|cx| crate::model_picker::ModelPicker::new(window, cx));
        cx.subscribe_in(
            &picker,
            window,
            |this, _, event: &crate::model_picker::PickerEvent, window, cx| {
                if let crate::model_picker::PickerEvent::Picked(id) = event {
                    this.pick_model(id.clone(), cx);
                }
                this.show_picker(false, window, cx);
            },
        )
        .detach();
        cx.observe(&scroller, |_, _, cx| cx.notify()).detach();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 12)
                .submit_on_enter(true)
                .placeholder("How can I help you today?")
        });
        cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } = event
                {
                    this.submit(window, cx);
                }
            },
        )
        .detach();
        Self {
            id,
            title: "New chat".into(),
            cwd,
            session: None,
            busy: false,
            baseline_taking: false,
            stopped: false,
            entries: Vec::new(),
            scroller,
            composer,
            held: String::new(),
            held_thought: false,
            revealing: false,
            loading_history: false,
            read_only: false,
            attachments: Vec::new(),
            preparing: None,
            edits: Default::default(),
            edits_changed: false,
            worktree: None,
            history: History::Complete,
            born: Vec::new(),
            fold_of: Vec::new(),
            folds: Vec::new(),
            turn_start: None,
            snapshot: None,
            patch: String::new(),
            patch_files: Vec::new(),
            views: Vec::new(),
            model: crate::agent_launch::DEFAULT_MODEL.into(),
            models: Default::default(),
            model_picked: false,
            picker,
            picker_open: false,
            picker_closed: None,
            decoding: 0,
            queued: None,
            working_since: None,
            ended: None,
            turn_failed: None,
            unqueue_pending: false,
            views_from_newer_pages: Default::default(),
            edited_before_baseline: false,
            diff_running: false,
            diff_stale: false,
        }
    }

    /// Mirrors the lifecycle's view of this chat.
    pub fn sync(&mut self, state: &sense_core::Chat, cx: &mut Context<Self>) {
        let was_busy = self.busy;
        self.busy = state.is_busy();
        if state.turn == sense_core::Turn::Cancelling {
            self.stopped = true;
        }
        self.track_status(was_busy);
        self.session = state.session.clone();
        self.cwd = state.cwd.clone();
        self.baseline_taking = state.baseline == sense_core::Baseline::Taking;
        if state.turn == sense_core::Turn::Cancelling {
            self.stopped = true;
        }
        if was_busy && !self.busy {
            self.flush_held(cx);
            // A stopped turn has no answer, whatever it last said.
            let stopped = std::mem::take(&mut self.stopped);
            if let Some(start) = self.turn_start.take()
                && !stopped
            {
                self.fold_turn(start, cx);
            }
            // The queued message goes now, unless the turn was stopped: then
            // it comes back to the composer, to be sent or changed.
            if stopped {
                self.unqueue_pending = self.queued.is_some();
            } else if let Some(queued) = self.queued.take() {
                self.send(queued.text, queued.attachments, cx);
            }
        }
        cx.notify();
    }

    /// Folds a finished turn: everything between its first entry and the
    /// agent's last message becomes one row. Notices stay out, so errors
    /// remain in view. A turn that ended without a final message, or that
    /// took no real steps, is left as is.
    fn fold_turn(&mut self, start: usize, cx: &mut Context<Self>) {
        let Some(last_answer) = (start + 1..self.entries.len())
            .rev()
            .find(|ix| matches!(self.entries[*ix], Entry::Agent(..)))
        else {
            return;
        };
        // Work after the last message means the turn ended without an
        // answer (stopped, or failed): there's nothing to fold under.
        let answered = self.entries[last_answer + 1..]
            .iter()
            .all(|entry| matches!(entry, Entry::Notice(_) | Entry::Diagnostic { .. }));
        if !answered {
            return;
        }
        let steps: Vec<usize> = (start + 1..last_answer)
            .filter(|ix| !matches!(self.entries[*ix], Entry::Notice(_)))
            .collect();
        let worked = steps
            .iter()
            .any(|ix| matches!(self.entries[*ix], Entry::Tool { .. } | Entry::Thought(..)));
        if steps.len() < 2 && !worked {
            return;
        }
        let (first, last) = (steps[0], *steps.last().unwrap());
        let took = self.born[last_answer].checked_duration_since(self.born[start]);
        let fold = self.folds.len();
        self.folds.push(Fold {
            first,
            last,
            steps: steps.len(),
            took: (!self.loading_history).then_some(took).flatten(),
            expanded: false,
        });
        for ix in steps {
            self.fold_of[ix] = Some(fold);
        }
        if !self.loading_history {
            self.scroller.update(cx, |state, cx| {
                _ = state.remeasure_items(first..last + 1, cx)
            });
        }
        cx.notify();
    }

    pub fn toggle_fold(&mut self, fold: usize, cx: &mut Context<Self>) {
        let Some(fold) = self.folds.get_mut(fold) else {
            return;
        };
        fold.expanded = !fold.expanded;
        let range = fold.first..fold.last + 1;
        self.scroller
            .update(cx, |state, cx| _ = state.remeasure_items(range, cx));
        cx.notify();
    }

    /// Where this chat keeps its snapshot's git data.
    pub fn snapshot_dir(&self) -> Option<PathBuf> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.git_dir().to_path_buf())
    }

    /// Snapshots `cwd` off the main thread; reports back as
    /// [`ChatEvent::BaselineTaken`].
    pub fn take_baseline(&mut self, cwd: PathBuf, cx: &mut Context<Self>) {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let git_dir = crate::snapshot::store_dir(&format!("{started}-{}", self.id));
        cx.spawn(async move |this, cx| {
            let taken = cx
                .background_spawn(async move { Snapshot::take(&cwd, &git_dir) })
                .await;
            _ = this.update(cx, |this, cx| match taken {
                Ok(snapshot) => {
                    this.snapshot = Some(Arc::new(snapshot));
                    if this.edited_before_baseline {
                        this.push(
                            Entry::Notice(
                                "The agent changed files before change tracking started; \
                                 those changes may be missing from the diff."
                                    .into(),
                            ),
                            cx,
                        );
                    }
                    cx.emit(ChatEvent::BaselineTaken(None));
                }
                Err(err) => cx.emit(ChatEvent::BaselineTaken(Some(err.to_string()))),
            });
        })
        .detach();
    }

    /// Recomputes the patch off the main thread. Requests that arrive while
    /// one is running collapse into a single rerun.
    pub fn refresh_patch(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        if self.diff_running {
            self.diff_stale = true;
            return;
        }
        self.diff_running = true;
        cx.spawn(async move |this, cx| {
            // Both the git run and the parse stay off the main thread.
            let result = cx
                .background_spawn(async move {
                    let patch = snapshot.diff()?;
                    let files = DiffFile::parse(&patch).unwrap_or_default();
                    anyhow::Ok((patch, files))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.diff_running = false;
                if let Ok((patch, files)) = result
                    && patch != this.patch
                {
                    this.patch = patch;
                    this.patch_files = files;
                    cx.emit(ChatEvent::PatchChanged);
                }
                if std::mem::take(&mut this.diff_stale) {
                    this.refresh_patch(cx);
                }
            });
        })
        .detach();
    }

    /// A subagent's own changes, from the edits it reported. Its folder is
    /// shared with the parent, so the folder's git diff would mix the two.
    fn record_edits(&mut self, update: &sense_agent::acp::ToolCallUpdate) {
        let mut changed = false;
        for item in update.fields.content.iter().flatten() {
            if let sense_agent::acp::ToolCallContent::Diff(diff) = item {
                let original = diff.old_text.clone().unwrap_or_default();
                let entry = self
                    .edits
                    .entry(diff.path.clone())
                    .or_insert((original, String::new()));
                entry.1 = diff.new_text.clone();
                changed = true;
            }
        }
        if changed {
            self.edits_changed = true;
        }
    }

    /// Turns the recorded edits into the diff panel's files, off the main
    /// thread.
    fn publish_edits(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.edits_changed) {
            return;
        }
        let edits = self.edits.clone();
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let (patch, files) = cx
                .background_spawn(async move {
                    let mut patch = String::new();
                    for (path, (old, new)) in &edits {
                        if old == new {
                            continue;
                        }
                        let name = path
                            .strip_prefix(&cwd)
                            .unwrap_or(path)
                            .display()
                            .to_string();
                        let diff = similar::TextDiff::from_lines(old, new);
                        let unified = diff
                            .unified_diff()
                            .context_radius(3)
                            .header(&format!("a/{name}"), &format!("b/{name}"))
                            .to_string();
                        patch.push_str(&unified);
                    }
                    let files = DiffFile::parse(&patch).unwrap_or_default();
                    (patch, files)
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.patch = patch;
                this.patch_files = files;
                cx.emit(ChatEvent::PatchChanged);
            });
        })
        .detach();
    }

    /// For a subagent: whether its parent reports it working.
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        let was_busy = std::mem::replace(&mut self.busy, running);
        self.track_status(was_busy);
        self.busy = was_busy;
        if self.busy && !running {
            self.flush_held(cx);
            if let Some(start) = self.turn_start.take() {
                self.fold_turn(start, cx);
            }
        }
        self.busy = running;
        cx.notify();
    }

    /// Adds one page of history above what's shown. `turns` is newest
    /// first, as fx pages it; each turn's updates are in their own order.
    pub fn add_history_page(
        &mut self,
        turns: Vec<Vec<SessionUpdate>>,
        older: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let first_page = matches!(self.history, History::Loading { first: true, .. });
        let shown = std::mem::take(&mut self.entries);
        let shown_born = std::mem::take(&mut self.born);
        let shown_fold_of = std::mem::take(&mut self.fold_of);
        let shown_folds = std::mem::take(&mut self.folds);
        let live_turn = self.turn_start.take();
        self.views_from_newer_pages = self.views.iter().map(|view| view.id.clone()).collect();
        self.loading_history = true;
        for turn in turns.into_iter().rev() {
            let start = self.entries.len();
            for update in turn {
                self.apply(update, cx);
            }
            self.flush_held(cx);
            self.turn_start = None;
            self.fold_turn(start, cx);
        }
        self.loading_history = false;
        let added = self.entries.len();
        // What was shown moves down by the page; its folds move with it.
        let page_folds = self.folds.len();
        self.folds.extend(shown_folds.into_iter().map(|mut fold| {
            fold.first += added;
            fold.last += added;
            fold
        }));
        self.fold_of.extend(
            shown_fold_of
                .into_iter()
                .map(|fold| fold.map(|f| f + page_folds)),
        );
        self.turn_start = live_turn.map(|start| start + added);
        // History was there all along: it doesn't fade in.
        let long_ago = Instant::now() - ROW_FADE;
        self.born.iter_mut().for_each(|born| *born = long_ago);
        self.entries.extend(shown);
        self.born.extend(shown_born);
        self.history = match older {
            Some(cursor) => History::More(cursor),
            None => History::Complete,
        };
        // Less than a screenful: there's nothing to scroll yet, so the next
        // page comes now rather than waiting for a scroll that can't happen.
        if matches!(self.history, History::More(_)) && self.entries.len() < SCREENFUL_ROWS {
            cx.emit(ChatEvent::NeedOlderHistory);
        }
        let total = self.entries.len();
        self.scroller.update(cx, |state, cx| {
            if first_page {
                state.reset(total, cx);
                state.scroll_to_end(cx);
            } else {
                // Keeps the reader's place while rows appear above.
                _ = state.prepend(added, cx);
            }
        });
        cx.notify();
    }

    /// The cursor for the next older page, if one should be fetched now.
    pub fn next_history_page(&mut self, first: bool) -> Option<Option<String>> {
        match (&self.history, first) {
            (History::NotLoaded, true) => {
                self.history = History::Loading {
                    first: true,
                    cursor: None,
                };
                Some(None)
            }
            (History::More(cursor), false) => {
                let cursor = cursor.clone();
                self.history = History::Loading {
                    first: false,
                    cursor: Some(cursor.clone()),
                };
                Some(Some(cursor))
            }
            _ => None,
        }
    }

    /// The page being waited for could not be read.
    pub fn history_failed(&mut self, error: &str, cx: &mut Context<Self>) {
        if let History::Loading { first, cursor } = &self.history {
            self.history = History::Failed {
                first: *first,
                cursor: cursor.clone(),
            };
            self.notice(format!("Couldn't load this chat's history: {error}"), cx);
        }
    }

    /// The page to ask for (again) now that the session store can be read:
    /// one that failed, or one asked for of a store that went away.
    pub fn history_to_retry(&mut self) -> Option<Option<String>> {
        match &self.history {
            History::Loading { first, cursor } | History::Failed { first, cursor } => {
                let (first, cursor) = (*first, cursor.clone());
                self.history = History::Loading {
                    first,
                    cursor: cursor.clone(),
                };
                Some(cursor)
            }
            _ => None,
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// Sends what's in the composer; while a turn runs, queues it to go
    /// when the turn is done (one message at most: with one waiting, the
    /// draft stays in the composer).
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if (text.is_empty() && self.attachments.is_empty())
            || self.preparing.is_some()
            || self.decoding > 0
            || self.read_only
        {
            return;
        }
        if self.busy && self.queued.is_some() {
            return;
        }
        let attachments = std::mem::take(&mut self.attachments);
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        if self.busy {
            self.queued = Some(Queued { text, attachments });
            cx.notify();
            return;
        }
        self.send(text, attachments, cx);
    }

    fn send(&mut self, text: String, attachments: Vec<Attachment>, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            self.title = text
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(48)
                .collect::<String>()
                .into();
            cx.emit(ChatEvent::TitleChanged);
        }
        let shown = attachments.iter().map(|a| a.thumb.clone()).collect();
        self.push(Entry::User(text.clone().into(), shown), cx);
        self.turn_start = Some(self.entries.len() - 1);
        let images = attachments
            .into_iter()
            .map(|a| sense_core::Image {
                mime: a.mime,
                bytes: a.bytes,
            })
            .collect();
        cx.emit(ChatEvent::Submit(sense_core::Prompt {
            text,
            images,
            model: Some(self.model.clone()),
        }));
    }

    /// Notes when a turn starts and how it ends. `self.busy` is already
    /// the new state; `self.stopped` says whether Stop was pressed.
    fn track_status(&mut self, was_busy: bool) {
        if !was_busy && self.busy {
            self.working_since = Some(Instant::now());
            self.turn_failed = None;
        } else if was_busy && !self.busy {
            let at = Instant::now();
            self.working_since = None;
            self.ended = Some(match self.turn_failed.take() {
                Some(error) => Status::Failed { at, error },
                None if self.stopped => Status::Stopped { at },
                None => Status::Done { at },
            });
        }
    }

    pub fn status(&self) -> Status {
        if let Some(reason) = &self.preparing {
            return Status::Preparing(reason.clone());
        }
        let asking = self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Permission {
                title,
                ask: Some(_),
                ..
            } => Some(title.clone()),
            _ => None,
        });
        if let Some(title) = asking {
            return Status::NeedsPermission(title);
        }
        if self.busy {
            return Status::Working {
                since: self.working_since.unwrap_or_else(Instant::now),
            };
        }
        self.ended.clone().unwrap_or(Status::Idle)
    }

    /// Option+Up: the queued message back in the composer, ahead of any
    /// draft typed since, with its images; it's no longer queued.
    pub fn unqueue(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(queued) = self.queued.take() else {
            return false;
        };
        let draft = self.composer.read(cx).value().to_string();
        let text = if draft.trim().is_empty() {
            queued.text
        } else {
            format!("{}\n{draft}", queued.text)
        };
        self.composer.update(cx, |state, cx| {
            state.set_value(&text, window, cx);
            state.focus(window, cx);
        });
        let typed_since = std::mem::take(&mut self.attachments);
        self.attachments = queued.attachments;
        self.attachments.extend(typed_since);
        cx.notify();
        true
    }

    /// Sends `text` as the user's message, as a click in a view does; the
    /// draft in the composer stays. Not while a turn runs.
    pub fn reply(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        if text.trim().is_empty() || self.busy || self.preparing.is_some() || self.read_only {
            return false;
        }
        self.push(Entry::User(text.clone().into(), Vec::new()), cx);
        self.turn_start = Some(self.entries.len() - 1);
        cx.emit(ChatEvent::Submit(sense_core::Prompt {
            text,
            images: Vec::new(),
            model: Some(self.model.clone()),
        }));
        true
    }

    /// A `show_view` call: the view it carries replaces any with its id.
    /// fx marks its own calls `_meta.fx.toolCall.view`; sessions from before
    /// it drew views name the app's old MCP server instead. Calls that
    /// aren't a view (the model hears why) are left as the tool rows they
    /// are.
    fn take_view(
        &mut self,
        meta: Option<&serde_json::Map<String, serde_json::Value>>,
        raw_input: Option<&serde_json::Value>,
        cx: &mut Context<Self>,
    ) {
        let call = meta
            .and_then(|meta| meta.get("fx"))
            .map(|fx| &fx["toolCall"]);
        let ours = call.is_some_and(|call| {
            call["view"] == true
                || (call["mcp"]["server"] == crate::view::LEGACY_SERVER
                    && call["mcp"]["tool"] == crate::view::TOOL)
        });
        let Some(view) = raw_input
            .filter(|_| ours)
            .and_then(|input| crate::view::View::parse(input).ok())
        else {
            return;
        };
        let id = view.id.clone();
        // History comes newest page first: a view already shown from a
        // newer page outranks this older version of it.
        if self.loading_history && self.views_from_newer_pages.contains(&id) {
            return;
        }
        match self.views.iter_mut().find(|shown| shown.id == id) {
            Some(shown) if *shown == view => return,
            Some(shown) => *shown = view,
            None => self.views.push(view),
        }
        cx.emit(ChatEvent::ViewShown(id));
    }

    /// Adds images to the next message: (mime type, bytes) each. Files that
    /// aren't images gpui can show are skipped.
    /// Decoding happens off the main thread; the images appear when ready.
    fn attach(&mut self, found: Vec<(String, Vec<u8>)>, cx: &mut Context<Self>) {
        self.decoding += 1;
        cx.spawn(async move |this, cx| {
            let decoded: Vec<Attachment> = cx
                .background_spawn(async move {
                    found
                        .into_iter()
                        .filter(|(mime, _)| image_format(mime).is_some())
                        .filter_map(|(mime, bytes)| {
                            let thumb = crate::thumb::thumbnail(&bytes)?;
                            Some(Attachment {
                                mime,
                                bytes: bytes.into(),
                                thumb,
                            })
                        })
                        .collect()
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.decoding -= 1;
                this.attachments.extend(decoded);
                cx.notify();
            });
        })
        .detach();
    }

    /// Attaches image files, as the picker does.
    #[cfg(feature = "automation")]
    pub fn attach_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let found = read_images(&paths);
        self.attach(found, cx);
    }

    /// Pastes a clipboard item into the composer: images attach; anything
    /// else is left to the text field (returns false). Automation calls it
    /// directly, so tests never touch the system clipboard.
    pub fn paste_item(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let found = images_on_clipboard(item);
        let consumed = !found.is_empty();
        self.attach(found, cx);
        consumed
    }

    fn pick_images(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            // Reading files stays off the main thread.
            let found = cx
                .background_spawn(async move { read_images(&paths) })
                .await;
            _ = this.update(cx, |this, cx| this.attach(found, cx));
        })
        .detach();
    }

    /// The composer's pick: this chat only, kept even when the default
    /// changes.
    pub fn pick_model(&mut self, id: String, cx: &mut Context<Self>) {
        self.model_picked = true;
        self.model = id;
        cx.notify();
    }

    /// A reopened thread: back on the model it last ran on, kept like a
    /// pick, so a new default doesn't move it.
    pub fn restore_model(&mut self, id: String, cx: &mut Context<Self>) {
        self.pick_model(id, cx);
    }

    /// Opens or closes the model picker; closing gives the composer back
    /// the cursor.
    pub fn show_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_open = open;
        if open {
            let (models, current) = (self.models.clone(), self.model.clone());
            self.picker
                .update(cx, |picker, cx| picker.open(models, current, window, cx));
        } else {
            self.composer.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// A new default reaches a chat that hasn't started and has no pick of
    /// its own; a running conversation keeps its model.
    pub fn follow_default(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.model_picked && self.entries.is_empty() && self.session.is_none() {
            self.model = id.to_string();
            cx.notify();
        }
    }

    /// Where the agent works, for the composer: a worktree's branch, or the
    /// folder with the home folder as `~`.
    fn place_label(&self) -> SharedString {
        if let Some((_, branch)) = &self.worktree {
            return branch.clone();
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        match home.and_then(|home| self.cwd.strip_prefix(home).ok().map(PathBuf::from)) {
            Some(rest) => format!("~/{}", rest.display()).into(),
            None => self.cwd.display().to_string().into(),
        }
    }

    /// What the in-flight indicator says while a turn runs.
    fn activity(&self) -> &'static str {
        if let Status::NeedsPermission(_) = self.status() {
            return "Waiting for you…";
        }
        match self.entries.last() {
            Some(Entry::Tool {
                status: ToolCallStatus::Pending | ToolCallStatus::InProgress,
                ..
            }) => "Working…",
            Some(Entry::Agent(..)) => "Writing…",
            _ => "Thinking…",
        }
    }

    pub fn notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.flush_held(cx);
        self.push(Entry::Notice(text.into()), cx);
    }

    pub fn ask_permission(&mut self, ask: PermissionAsk, cx: &mut Context<Self>) {
        let title = ask
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Run tool".into());
        let options = ask
            .options
            .iter()
            .map(|option| {
                let allow = matches!(
                    option.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                );
                (SharedString::from(option.name.clone()), allow)
            })
            .collect();
        self.push(
            Entry::Permission {
                title: title.into(),
                options,
                ask: Some(Box::new(ask)),
            },
            cx,
        );
    }

    pub fn apply(&mut self, update: SessionUpdate, cx: &mut Context<Self>) {
        let is_text = matches!(
            update,
            SessionUpdate::AgentMessageChunk(_) | SessionUpdate::AgentThoughtChunk(_)
        );
        if !is_text {
            // Anything else ends the current run of text, partial line or not.
            self.flush_held(cx);
        }
        match update {
            // History arrives whole: no paced reveal.
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    if self.loading_history {
                        self.append_text(false, &text.text, cx);
                    } else {
                        self.hold(false, &text.text, cx);
                    }
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    if self.loading_history {
                        self.append_text(true, &text.text, cx);
                    } else {
                        self.hold(true, &text.text, cx);
                    }
                }
            }
            // Only history replays the user's own messages.
            SessionUpdate::UserMessageChunk(chunk) => match chunk.content {
                ContentBlock::Text(text) => {
                    self.push(Entry::User(text.text.into(), Vec::new()), cx);
                    // History, or a subagent's delegation: a turn starts here.
                    self.turn_start = Some(self.entries.len() - 1);
                }
                // fx sends each image of a message as its own chunk, after
                // the text: it joins the message it belongs to. A message of
                // only images starts with one.
                ContentBlock::Image(image) => {
                    let Some(image) = decode_image(&image.data, &image.mime_type) else {
                        return;
                    };
                    if let Some(Entry::User(_, images)) = self.entries.last_mut() {
                        images.push(image);
                    } else {
                        self.push(Entry::User(SharedString::default(), vec![image]), cx);
                        self.turn_start = Some(self.entries.len() - 1);
                    }
                }
                _ => {}
            },
            SessionUpdate::ToolCall(call)
                if self.baseline_taking
                    && !self.loading_history
                    && !self.edited_before_baseline
                    && matches!(
                        call.kind,
                        ToolKind::Edit | ToolKind::Delete | ToolKind::Move | ToolKind::Execute
                    ) =>
            {
                self.edited_before_baseline = true;
                self.apply(SessionUpdate::ToolCall(call), cx);
            }
            SessionUpdate::ToolCall(call) => {
                self.take_view(call.meta.as_ref(), call.raw_input.as_ref(), cx);
                self.push(
                    Entry::Tool {
                        id: call.tool_call_id,
                        title: call.title.into(),
                        status: call.status,
                    },
                    cx,
                )
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.take_view(update.meta.as_ref(), update.fields.raw_input.as_ref(), cx);
                if self.read_only {
                    self.record_edits(&update);
                    self.publish_edits(cx);
                }
                let found = self
                    .entries
                    .iter_mut()
                    .enumerate()
                    .rev()
                    .find_map(|(ix, entry)| match entry {
                        Entry::Tool { id, title, status } if *id == update.tool_call_id => {
                            if let Some(new_title) = &update.fields.title {
                                *title = new_title.clone().into();
                            }
                            if let Some(new_status) = update.fields.status {
                                *status = new_status;
                            }
                            Some(ix)
                        }
                        _ => None,
                    });
                if let Some(ix) = found
                    && !self.loading_history
                {
                    self.scroller
                        .update(cx, |state, cx| _ = state.remeasure_items(ix..ix + 1, cx));
                    cx.notify();
                }
            }
            SessionUpdate::Notice(notice) => {
                let entry = match notice.severity {
                    NoticeSeverity::Info => {
                        // The title is shown on one line; whatever doesn't
                        // fit stays readable by expanding the row.
                        let title = notice.title.trim().to_string();
                        let details = match notice.description {
                            Some(details) => Some(format!("{title}\n\n{details}")),
                            None if title.contains('\n') || title.len() > 100 => {
                                Some(title.clone())
                            }
                            None => None,
                        };
                        Entry::Diagnostic {
                            title: title.lines().next().unwrap_or_default().to_string().into(),
                            details: details.map(Into::into),
                            expanded: false,
                        }
                    }
                    _ => Entry::Notice(
                        match notice.description {
                            Some(details) => format!("{}\n{details}", notice.title),
                            None => notice.title,
                        }
                        .into(),
                    ),
                };
                self.push(entry, cx);
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                if let Some(title) = info.title.value().cloned() {
                    self.title = title.into();
                    cx.emit(ChatEvent::TitleChanged);
                }
            }
            _ => {}
        }
    }

    /// Buffers streamed text and reveals it at an even pace. Network chunks
    /// arrive in bursts; showing them as they land looks jerky. A frame
    /// timer drains the buffer by a share of what's waiting, so the reveal
    /// stays smooth and never trails the stream by more than a few frames.
    fn hold(&mut self, thought: bool, text: &str, cx: &mut Context<Self>) {
        if thought != self.held_thought {
            self.flush_held(cx);
            self.held_thought = thought;
        }
        self.held.push_str(text);
        if self.revealing {
            return;
        }
        self.revealing = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REVEAL_FRAME).await;
                let more = this
                    .update(cx, |this, cx| this.reveal_some(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    /// Reveals the next slice of held text; false once there is none left.
    fn reveal_some(&mut self, cx: &mut Context<Self>) -> bool {
        if self.held.is_empty() {
            self.revealing = false;
            return false;
        }
        let end = reveal_cut(&self.held);
        let slice: String = self.held.drain(..end).collect();
        self.append_text(self.held_thought, &slice, cx);
        true
    }

    fn flush_held(&mut self, cx: &mut Context<Self>) {
        if !self.held.is_empty() {
            let rest = std::mem::take(&mut self.held);
            self.append_text(self.held_thought, &rest, cx);
        }
    }

    fn append_text(&mut self, thought: bool, text: &str, cx: &mut Context<Self>) {
        let last = self.entries.len().checked_sub(1);
        let target = match self.entries.last_mut() {
            Some(Entry::Agent(state, source)) if !thought => {
                source.push_str(text);
                Some(state.clone())
            }
            Some(Entry::Thought(state, source)) if thought => {
                source.push_str(text);
                Some(state.clone())
            }
            _ => None,
        };
        match (target, last) {
            (Some(state), Some(ix)) => {
                state.update(cx, |state, cx| state.push_str(text, cx));
                if !self.loading_history {
                    self.scroller.update(cx, |scroller, cx| {
                        _ = scroller.remeasure_items(ix..ix + 1, cx)
                    });
                }
                cx.notify();
            }
            _ => {
                let state = cx.new(|cx| TextViewState::markdown(text, cx));
                let source = text.to_string();
                self.push(
                    if thought {
                        Entry::Thought(state, source)
                    } else {
                        Entry::Agent(state, source)
                    },
                    cx,
                );
            }
        }
    }

    fn push(&mut self, entry: Entry, cx: &mut Context<Self>) {
        self.entries.push(entry);
        self.born.push(Instant::now());
        self.fold_of.push(None);
        // A history page sets the list's rows once it's all in place.
        if !self.loading_history {
            self.scroller
                .update(cx, |state, cx| _ = state.append(1, cx));
        }
        cx.notify();
    }

    #[cfg(feature = "automation")]
    pub fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    #[cfg(feature = "automation")]
    /// Answers the most recent unanswered permission request.
    pub fn answer_latest(&mut self, choice: Option<usize>, cx: &mut Context<Self>) -> bool {
        let pending = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::Permission { ask: Some(_), .. }));
        if let Some(ix) = pending {
            self.answer(ix, choice, cx);
        }
        pending.is_some()
    }

    #[cfg(feature = "automation")]
    /// A plain description of the transcript, for automation and tests.
    pub fn snapshot(&self, cx: &App) -> serde_json::Value {
        let entries: Vec<_> = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::User(text, images) => {
                    serde_json::json!({ "user": text.as_ref(), "images": images.len() })
                }
                Entry::Agent(_, source) => serde_json::json!({ "agent": source }),
                Entry::Thought(_, source) => serde_json::json!({ "thought": source }),
                Entry::Tool { title, status, .. } => {
                    serde_json::json!({ "tool": title.as_ref(), "status": format!("{status:?}") })
                }
                Entry::Permission { title, ask, .. } => {
                    serde_json::json!({ "permission": title.as_ref(), "pending": ask.is_some() })
                }
                Entry::Notice(text) => serde_json::json!({ "notice": text.as_ref() }),
                Entry::Diagnostic {
                    title, expanded, ..
                } => {
                    serde_json::json!({ "diagnostic": title.as_ref(), "expanded": expanded })
                }
            })
            .collect();
        serde_json::json!({
            "place": self.place_label().to_string(),
            "model": self.model,
            "picker": self.picker_open.then(|| self.picker.read(cx).snapshot()),
            "title": self.title.as_ref(),
            "session": self.session,
            "busy": self.busy,
            "attachments": self.attachments.iter().map(|a| a.mime.clone()).collect::<Vec<_>>(),
            "queued": self.queued.as_ref().map(|q| serde_json::json!({ "text": q.text, "images": q.attachments.len() })),
            "draft": self.composer.read(cx).value().to_string(),
            "read_only": self.read_only,
            "folds": self.folds.iter().map(|fold| serde_json::json!({
                "steps": fold.steps,
                "expanded": fold.expanded,
                "first": fold.first,
                "last": fold.last,
            })).collect::<Vec<_>>(),
            "draft": self.composer.read(cx).value().as_ref(),
            "entries": entries,
            "patch": self.patch,
            "snapshot_dir": self.snapshot_dir(),
        })
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(Entry::Diagnostic { expanded, .. }) = self.entries.get_mut(ix) {
            *expanded = !*expanded;
            self.scroller
                .update(cx, |state, cx| _ = state.remeasure_items(ix..ix + 1, cx));
            cx.notify();
        }
    }

    fn answer(&mut self, ix: usize, choice: Option<usize>, cx: &mut Context<Self>) {
        if let Some(Entry::Permission { ask, .. }) = self.entries.get_mut(ix)
            && let Some(ask) = ask.take()
        {
            ask.answer(choice);
        }
        self.scroller
            .update(cx, |state, cx| _ = state.remeasure_items(ix..ix + 1, cx));
        cx.notify();
    }

    fn render_entry(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The list and the entries change together; a row past the end would
        // mean they didn't, and must not take the app down.
        if ix >= self.entries.len() {
            debug_assert!(false, "row {ix} of {}", self.entries.len());
            return div().into_any_element();
        }
        if let Some(fold_ix) = self.fold_of[ix] {
            let fold = &self.folds[fold_ix];
            if !fold.expanded && ix != fold.first {
                return div().into_any_element();
            }
            if ix == fold.first {
                let theme = cx.theme();
                let label = match fold.took {
                    Some(took) => {
                        format!("Worked for {} · {} steps", short_duration(took), fold.steps)
                    }
                    None => format!("{} steps", fold.steps),
                };
                let summary = h_flex()
                    .id(("fold", fold_ix))
                    .w_full()
                    .max_w(px(760.))
                    .mx_auto()
                    .px_6()
                    .pt_3()
                    .pb_1()
                    .gap_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|this| this.text_color(theme.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(fold_ix, cx)))
                    .child(div().child(label))
                    .child(
                        Icon::new(if fold.expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    );
                if !fold.expanded {
                    return summary.into_any_element();
                }
                let content = self.render_row(ix, window, cx);
                return v_flex()
                    .w_full()
                    .child(summary)
                    .child(content)
                    .into_any_element();
            }
        }
        self.render_row(ix, window, cx)
    }

    fn render_row(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Tool rows and notices fade in; streamed text has its own fade.
        let fades = matches!(
            self.entries[ix],
            Entry::Tool { .. }
                | Entry::Notice(_)
                | Entry::Diagnostic { .. }
                | Entry::Permission { .. }
        );
        let age = self.born.get(ix).map(Instant::elapsed).unwrap_or(ROW_FADE);
        let opacity = if fades && age < ROW_FADE {
            window.request_animation_frame();
            age.as_secs_f32() / ROW_FADE.as_secs_f32()
        } else {
            1.
        };
        let theme = cx.theme();
        let row = div()
            .w_full()
            .max_w(px(760.))
            .mx_auto()
            .px_6()
            .opacity(opacity);
        // Tool rows sit tight under each other; everything else gets air.
        let row = match &self.entries[ix] {
            Entry::Tool { .. } | Entry::Diagnostic { .. } => row.py_1(),
            _ => row.pt_4().pb_2(),
        };
        match &self.entries[ix] {
            Entry::User(text, images) => row
                .group(MESSAGE_GROUP)
                .child(
                    v_flex()
                        .items_end()
                        .gap_1()
                        .when(!images.is_empty(), |this| {
                            this.child(h_flex().gap_1().children(images.iter().map(|thumb| {
                                img(thumb.image.clone())
                                    .w(thumb.shown.width)
                                    .h(thumb.shown.height)
                                    .rounded(px(10.))
                            })))
                        })
                        .when(!text.is_empty(), |this| {
                            this.child(
                                div()
                                    .max_w(px(560.))
                                    .px_4()
                                    .py_2()
                                    .rounded(px(14.))
                                    .bg(theme.secondary)
                                    .child(text.clone()),
                            )
                        })
                        .child(copy_button(ix, text.clone())),
                )
                .into_any_element(),
            Entry::Agent(state, source) => row
                .group(MESSAGE_GROUP)
                .child(markdown(state).stream_fade(true))
                .child(copy_button(ix, source.clone().into()))
                .into_any_element(),
            Entry::Thought(state, _) => row
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(TextView::new(state).markdown_extensions(crate::math::extensions()))
                .into_any_element(),
            Entry::Tool { title, status, .. } => {
                // A running tool spins: a still icon reads as stalled during
                // a long command.
                let (icon, color) = match status {
                    ToolCallStatus::Completed => (Some(IconName::Check), theme.muted_foreground),
                    ToolCallStatus::Failed => (Some(IconName::CircleX), theme.danger),
                    _ => (None, theme.muted_foreground),
                };
                row.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(color)
                        .map(|row| match icon {
                            Some(icon) => row.child(gpui_kit::component::Icon::new(icon).small()),
                            None => row.child(Spinner::new().small()),
                        })
                        // One line: a subagent's title can be its whole task.
                        .child(div().min_w_0().flex_1().truncate().child(title.clone())),
                )
                .into_any_element()
            }
            Entry::Permission {
                title,
                options,
                ask,
            } => {
                let pending = ask.is_some();
                row.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(theme.border)
                        .child(div().text_sm().child(title.clone()))
                        .child(h_flex().gap_2().children(options.iter().enumerate().map(
                            |(choice, (label, allow))| {
                                let button = Button::new(("perm", ix * 16 + choice))
                                    .small()
                                    .label(label.clone())
                                    .disabled(!pending)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer(ix, Some(choice), cx)
                                    }));
                                if *allow {
                                    button.primary()
                                } else {
                                    button.outline()
                                }
                            },
                        ))),
                )
                .into_any_element()
            }
            Entry::Notice(text) => row
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.border)
                        .child(text.clone()),
                )
                .into_any_element(),
            Entry::Diagnostic {
                title,
                details,
                expanded,
            } => {
                let expandable = details.is_some();
                row.text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        h_flex()
                            .id(("diagnostic", ix))
                            .gap_1()
                            .when(expandable, |this| {
                                this.cursor_pointer().on_click(
                                    cx.listener(move |this, _, _, cx| this.toggle(ix, cx)),
                                )
                            })
                            // A chevron only where there is more to see.
                            .child(
                                Icon::new(match (expandable, *expanded) {
                                    (false, _) => IconName::Info,
                                    (true, true) => IconName::ChevronDown,
                                    (true, false) => IconName::ChevronRight,
                                })
                                .xsmall(),
                            )
                            .child(div().min_w_0().flex_1().truncate().child(title.clone())),
                    )
                    .when_some(details.clone().filter(|_| *expanded), |this, details| {
                        this.child(div().pl_4().pt_1().child(details))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Near the top of what's loaded, with more history behind it: fetch
        // the next older page (the state turns to Loading, so this asks once).
        if matches!(self.history, History::More(_))
            && self.scroller.read(cx).distance_from_top() < HISTORY_PREFETCH
        {
            cx.defer_in(window, |_, _, cx| cx.emit(ChatEvent::NeedOlderHistory));
        }
        if std::mem::take(&mut self.unqueue_pending) {
            self.unqueue(window, cx);
        }
        let view = cx.entity();
        let empty = self.entries.is_empty();
        let composer = div()
            .w_full()
            .max_w(px(760.))
            .mx_auto()
            .px_6()
            .pb_6()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .rounded(px(18.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    // Option+Up takes the queued message back to edit.
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        let keystroke = &event.keystroke;
                        if keystroke.key == "up"
                            && keystroke.modifiers.alt
                            && !keystroke.modifiers.platform
                            && !keystroke.modifiers.control
                            && this.unqueue(window, cx)
                        {
                            cx.stop_propagation();
                        }
                    }))
                    .when_some(self.queued.as_ref(), |this, queued| {
                        let theme = cx.theme();
                        let images = queued.attachments.len();
                        this.child(
                            h_flex()
                                .id("queued")
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.border)
                                .text_sm()
                                .child(div().text_color(theme.muted_foreground).child("Queued"))
                                .child(div().flex_1().min_w_0().truncate().child(
                                    queued.text.lines().next().unwrap_or_default().to_string(),
                                ))
                                .when(images > 0, |this| {
                                    this.child(div().text_color(theme.muted_foreground).child(
                                        format!(
                                            "{images} image{}",
                                            if images == 1 { "" } else { "s" }
                                        ),
                                    ))
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("⌥↑ to edit"),
                                ),
                        )
                    })
                    .when(!self.attachments.is_empty(), |this| {
                        this.child(h_flex().gap_2().children(
                            self.attachments.iter().enumerate().map(|(n, attachment)| {
                                div()
                                    .relative()
                                    .group("attachment")
                                    .child({
                                        // As tall as a chip, as wide as its shape.
                                        let shown = attachment.thumb.shown;
                                        let scale = 56. / f32::from(shown.height);
                                        img(attachment.thumb.image.clone())
                                            .h(px(56.))
                                            .w(shown.width * scale)
                                            .rounded(px(8.))
                                    })
                                    .child(
                                        div()
                                            .absolute()
                                            .top(px(-6.))
                                            .right(px(-6.))
                                            .invisible()
                                            .group_hover("attachment", |s| s.visible())
                                            .child(
                                                Button::new(("remove-image", n))
                                                    .xsmall()
                                                    .icon(IconName::Close)
                                                    .rounded(cx.theme().radius_full())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            if n < this.attachments.len() {
                                                                this.attachments.remove(n);
                                                            }
                                                            cx.notify();
                                                        },
                                                    )),
                                            ),
                                    )
                            }),
                        ))
                    })
                    .child(Textarea::new(&self.composer).appearance(false).on_paste({
                        // Images on the clipboard (copied images, or copied
                        // image files) attach instead of pasting as text.
                        // The same path automation pastes through.
                        let view = cx.entity().downgrade();
                        move |item, _, cx| {
                            view.update(cx, |this, cx| this.paste_item(item, cx))
                                .unwrap_or(false)
                        }
                    }))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .gap_2()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        Button::new("attach")
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Plus)
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.pick_images(cx)),
                                            ),
                                    )
                                    .child(
                                        // Where the agent works, shortened to
                                        // fit: a worktree by its branch.
                                        h_flex()
                                            .min_w_0()
                                            .gap_1()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(Icon::new(IconName::Folder).xsmall())
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(self.place_label()),
                                            ),
                                    ),
                            )
                            .child(
                                // The model this chat answers with: a search
                                // over OpenRouter's list, opened above it.
                                div()
                                    .relative()
                                    .child(
                                        Button::new("model")
                                            .ghost()
                                            .xsmall()
                                            .label(crate::models::label(&self.models, &self.model))
                                            .disabled(self.read_only)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                // A click here while it's open
                                                // closed it on the way down.
                                                let just_closed =
                                                    this.picker_closed.is_some_and(|at| {
                                                        at.elapsed() < Duration::from_millis(300)
                                                    });
                                                if !just_closed {
                                                    this.show_picker(true, window, cx);
                                                }
                                            })),
                                    )
                                    .when(self.picker_open, |this| {
                                        let theme = cx.theme();
                                        this.child(deferred(
                                            div()
                                                .id("model-picker")
                                                .absolute()
                                                .bottom(px(32.))
                                                .right_0()
                                                .p_2()
                                                .rounded(theme.radius_lg)
                                                .border_1()
                                                .border_color(theme.border)
                                                .bg(theme.popover)
                                                .shadow_lg()
                                                .occlude()
                                                .on_mouse_down_out(cx.listener(
                                                    |this, _, window, cx| {
                                                        this.picker_closed = Some(Instant::now());
                                                        this.show_picker(false, window, cx)
                                                    },
                                                ))
                                                .child(self.picker.clone()),
                                        ))
                                    }),
                            )
                            .child(if self.busy {
                                Button::new("stop")
                                    .icon(IconName::Square)
                                    .small()
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::Stop)))
                            } else if let Some(reason) = self.preparing.clone() {
                                Button::new("preparing")
                                    .small()
                                    .loading(true)
                                    .label(reason)
                                    .disabled(true)
                            } else {
                                Button::new("send")
                                    .primary()
                                    .icon(IconName::ArrowUp)
                                    .small()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    )
                            }),
                    ),
            );

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .when(empty, |this| {
                this.justify_center().child(
                    div()
                        .pb_6()
                        .text_center()
                        .text_3xl()
                        .text_color(cx.theme().foreground)
                        .child("What are we building?"),
                )
            })
            .when(!empty, |this| {
                let scrolled_up = self.scroller.read(cx).is_scrolled_up();
                let scroller = self.scroller.clone();
                this.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            MessageScroller::new(
                                "transcript",
                                self.scroller.clone(),
                                move |ix, window, cx| {
                                    view.update(cx, |this, cx| this.render_entry(ix, window, cx))
                                },
                            )
                            // Replaced below: the built-in one always carries a tooltip.
                            .jump_button(false)
                            .with_row_style(StyleRefinement::default().pb_0())
                            .with_bottom_fade(cx.theme().background)
                            .size_full(),
                        )
                        .when(scrolled_up, |this| {
                            this.child(
                                h_flex()
                                    .absolute()
                                    .bottom_2()
                                    .left_0()
                                    .right_0()
                                    .justify_center()
                                    .child(
                                        Button::new("jump-to-latest")
                                            .icon(IconName::ArrowDown)
                                            .rounded(cx.theme().radius_full())
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .bg(cx.theme().background)
                                            .on_click(move |_, _, cx| {
                                                scroller.update(cx, |state, cx| {
                                                    state.scroll_to_end(cx)
                                                });
                                            }),
                                    ),
                            )
                        }),
                )
                // The slot is always reserved so the transcript doesn't jump
                // when a turn starts or ends.
                .child(
                    h_flex()
                        .w_full()
                        .max_w(px(760.))
                        .mx_auto()
                        .px_6()
                        .h(px(32.))
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .when(self.busy, |this| {
                            this.child(Spinner::new().small())
                                .child(ShimmerText::new(self.activity()))
                        }),
                )
            })
            // A subagent's transcript is read-only: fx doesn't take input
            // for a child from the user.
            .when(!self.read_only, |this| this.child(composer))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would shadow `#[test]`.
    use super::reveal_cut;
    use proptest::prelude::*;

    proptest! {
        /// Found by a crash: cutting inside a multi-byte character panicked.
        #[test]
        fn reveal_cuts_on_character_boundaries(held in "\\PC{0,400}") {
            let end = reveal_cut(&held);
            prop_assert!(held.is_char_boundary(end));
            prop_assert!(end <= held.len());
            prop_assert!(held.is_empty() || end > 0, "an empty reveal would stall");
        }
    }

    #[test]
    fn reveal_handles_the_text_that_crashed() {
        for held in [
            "Subagent · Find calc.py",
            "··········",
            "a\u{a0}b",
            "💥💥💥 boom",
            "é",
        ] {
            let end = reveal_cut(held);
            assert!(held.is_char_boundary(end), "{held:?} cut at {end}");
        }
    }
}
