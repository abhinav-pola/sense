//! The window: chats on the left, the active chat in the middle and the
//! right panel when the session has something to show.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use sense_agent::acp::{SessionId, SessionInfo, SessionUpdate, ToolCallStatus};
use sense_agent::{AgentHandle, Event};
use sense_core::{AgentId, ChatId, Command, Effect, Input, Lifecycle, Notice};

use crate::agent_launch;
use crate::chat::{ChatEvent, ChatView};
use crate::library::{Appearance, FolderId, Library, Place, ProjectId};
use crate::panel::RightPanel;

const SIDEBAR_WIDTH: f32 = 260.;
const SIDEBAR_MIN: f32 = 180.;
const SIDEBAR_MAX: f32 = 520.;
const PANEL_WIDTH: f32 = 480.;
const PANEL_MIN: f32 = 280.;
/// The chat keeps at least this much when either side is dragged wider.
const CHAT_MIN: f32 = 360.;
/// The grab area straddling each divider.
const DIVIDER_GRAB: f32 = 8.;
const TRAFFIC_LIGHT_CLEARANCE: f32 = 40.;
/// How much of the sidebar's color covers the blurred desktop behind it.
const SIDEBAR_TINT: f32 = 0.67;
/// Turns per history page: a screenful or two of a typical chat.
const HISTORY_PAGE: u32 = 30;

/// A subagent working for a chat in this run. Its transcript is read-only;
/// fx owns the conversation and it lives in the parent's context.
struct Child {
    session: String,
    view: Entity<ChatView>,
    running: bool,
    /// Hidden from the sidebar; still part of the parent's context.
    hidden: bool,
}

/// A chat's subagents, shown under it in the sidebar.
#[derive(Default)]
struct Family {
    collapsed: bool,
    children: Vec<Child>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadMode {
    CurrentBranch,
    Worktree,
}

/// A divider being dragged; renders nothing, the panes are the preview.
#[derive(Clone, Copy)]
enum Divider {
    Sidebar,
    Panel,
}

impl Render for Divider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A thread being dragged in the sidebar; also its drag preview. Named by
/// what it is rather than its row, which can move while it's dragged.
#[derive(Clone)]
struct DraggedThread {
    thread: ThreadKey,
    title: SharedString,
}

#[derive(Clone, PartialEq, Eq)]
enum ThreadKey {
    Chat(ChatId),
    Past(String),
}

impl Render for DraggedThread {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().sidebar_accent)
            .text_sm()
            .child(self.title.clone())
    }
}

fn section_header(
    label: &'static str,
    action: Option<AnyElement>,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    h_flex()
        .w_full()
        .px_2()
        .pt_4()
        .pb_1()
        .justify_between()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
        .children(action)
}

/// Removes a deleted thread's worktree in the background.
fn remove_worktree(path: PathBuf, cx: &mut App) {
    cx.background_spawn(async move {
        if let Err(err) = crate::worktree::remove(&path) {
            eprintln!("couldn't remove worktree {}: {err:#}", path.display());
        }
    })
    .detach();
}

fn library_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/Sense/library.json")
}

#[derive(Clone)]
enum Slot {
    Live(Entity<ChatView>),
    Past(PastSession),
}

#[derive(Clone)]
struct PastSession {
    session: String,
    cwd: PathBuf,
    title: SharedString,
}

impl Slot {
    fn view(&self) -> Option<&Entity<ChatView>> {
        match self {
            Slot::Live(view) => Some(view),
            Slot::Past(_) => None,
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            Slot::Live(view) => view.read(cx).title.clone(),
            Slot::Past(past) => past.title.clone(),
        }
    }
}

enum Account {
    Unknown,
    SignedOut,
    SigningIn,
    SignedIn { from_env: bool },
}

pub struct Workspace {
    /// Decides everything about chats, sessions, turns and agents; the
    /// workspace feeds it inputs and carries out the effects it returns.
    life: Lifecycle,
    /// The sidebar, newest first: open chats, and past sessions that only
    /// become chats (with a view and, when used, an agent) once opened.
    slots: Vec<Slot>,
    active: usize,
    next_chat: ChatId,
    /// Running agents, including ones told to stop that haven't exited yet.
    agents: HashMap<AgentId, AgentHandle>,
    account: Account,
    panel: Entity<RightPanel>,
    panel_open: bool,
    /// The text size just changed to, and when: shown briefly.
    text_size_badge: Option<(f32, std::time::Instant)>,
    /// The settings menu's search for the default model.
    default_picker: Entity<crate::model_picker::ModelPicker>,
    default_picker_open: bool,
    /// OpenRouter's models, shared with every chat's picker.
    models: std::sync::Arc<Vec<crate::models::Model>>,
    refreshing_models: bool,
    /// The sidebar row under the pointer: a thread, or one of its
    /// subagents. Its status shows in the sidebar's top corner.
    hovered: Option<(usize, Option<usize>)>,
    /// Redraws once a second while a working thread's time is shown.
    status_tick: Option<Task<()>>,
    /// The status last shown, for automated checks.
    status_shown: Option<String>,
    /// Pane widths, set by dragging the dividers.
    sidebar_width: f32,
    panel_width: f32,
    /// The agent is a recorded session played back, not fx.
    replaying: bool,
    librarian: Option<AgentHandle>,
    /// History pages fetched ahead of a chat opening, by session.
    history_cache: HashMap<String, (Vec<Vec<SessionUpdate>>, Option<String>)>,
    history_requested: std::collections::HashSet<String>,
    /// Subagents by parent chat, for this run only: reopening a past
    /// session doesn't bring them back.
    families: HashMap<ChatId, Family>,
    library: Library,
    /// Hands each changed library to the one task that writes it, so saves
    /// land in order and never overlap.
    library_saves: async_channel::Sender<Library>,
    /// Where new threads go, until fx gives them a session id to save under.
    pending_place: HashMap<ChatId, Place>,
    folded_projects: std::collections::HashSet<ProjectId>,
    folded_folders: std::collections::HashSet<FolderId>,
    renaming: Option<(FolderId, Entity<InputState>)>,
    /// The subagent being viewed, if any; always one of the active chat's.
    active_child: Option<Entity<ChatView>>,
    _tasks: Vec<Task<()>>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (library_saves, saves) = async_channel::unbounded::<Library>();
        let saver = cx.background_spawn(async move {
            let path = library_path();
            while let Ok(mut library) = saves.recv().await {
                // Only the newest of any that queued up meanwhile matters.
                while let Ok(newer) = saves.try_recv() {
                    library = newer;
                }
                _ = library.save(&path);
            }
        });
        let mut this = Self {
            life: Lifecycle::new(),
            slots: Vec::new(),
            active: 0,
            next_chat: 1,
            agents: HashMap::new(),
            account: Account::Unknown,
            panel: cx.new(RightPanel::new),
            panel_open: false,
            text_size_badge: None,
            hovered: None,
            status_tick: None,
            status_shown: None,
            models: Default::default(),
            default_picker: cx.new(|cx| crate::model_picker::ModelPicker::new(window, cx)),
            default_picker_open: false,
            refreshing_models: false,
            sidebar_width: SIDEBAR_WIDTH,
            panel_width: PANEL_WIDTH,
            replaying: agent_launch::replay_launch().is_some(),
            librarian: None,
            history_cache: HashMap::new(),
            history_requested: Default::default(),
            families: HashMap::new(),
            // Read before the first frame: it's a few kilobytes, and
            // loading it later would race with what the user does first.
            library: Library::load(&library_path()),
            library_saves,
            pending_place: HashMap::new(),
            folded_projects: Default::default(),
            folded_folders: Default::default(),
            renaming: None,
            active_child: None,
            _tasks: Vec::new(),
        };
        cx.subscribe_in(
            &this.default_picker.clone(),
            window,
            |this, _, event: &crate::model_picker::PickerEvent, window, cx| {
                if let crate::model_picker::PickerEvent::Picked(id) = event {
                    this.set_default_model(id.clone(), cx);
                }
                this.show_default_picker(false, window, cx);
            },
        )
        .detach();
        // A click in one of the agent's views answers it as the user.
        cx.subscribe(&this.panel, |this, _, reply: &crate::panel::Reply, cx| {
            if let Some(chat) = this.active_view() {
                chat.update(cx, |chat, cx| chat.reply(reply.0.clone(), cx));
            }
        })
        .detach();
        this.models = std::sync::Arc::new(this.library.models.clone());
        // Until the first refresh, the list built into the app; read after
        // the first frame, not before it.
        if this.models.is_empty() {
            let seed = cx.spawn(async move |this, cx| {
                let seed = cx.background_spawn(async { crate::models::seed() }).await;
                _ = this.update(cx, |this, cx| {
                    if this.models.is_empty() {
                        this.share_models(seed, cx);
                    }
                });
            });
            this._tasks.push(seed);
        }
        if let Some(size) = this.library.text_size {
            crate::set_text_size(cx, |_| f32::from(size));
        }
        this.apply_appearance(window, cx);
        // Following the system: its light/dark switch switches this too.
        cx.observe_window_appearance(window, |this, window, cx| this.apply_appearance(window, cx))
            .detach();
        // A text size change (cmd +/−) shows its size for a moment and is
        // kept for next time.
        cx.observe_global_in::<gpui_kit::component::Theme>(window, |this: &mut Self, _, cx| {
            let size = f32::from(cx.theme().font_size);
            let saved = this.library.text_size.map(f32::from);
            if saved.unwrap_or(crate::TEXT_SIZE_DEFAULT) != size {
                this.library.text_size = (size != crate::TEXT_SIZE_DEFAULT).then_some(size as u16);
                this.save_library(cx);
                this.text_size_badge = Some((size, std::time::Instant::now()));
                cx.notify();
            }
        })
        .detach();
        this.new_chat(window, cx);
        // The Keychain read and the agent start happen after the first frame.
        // Startup benchmarks skip them: they'd only add noise and stray fx
        // processes to a measurement of the first frame.
        let benchmarking = std::env::var_os("SENSE_EXIT_AFTER_FIRST_FRAME").is_some();
        let load = cx.spawn_in(window, async move |this, cx| {
            if benchmarking {
                return;
            }
            let account = cx.background_spawn(async { load_account() }).await;
            _ = this.update_in(cx, |this, window, cx| {
                this.account_loaded(account, window, cx)
            });
        });
        this._tasks.push(load);
        this._tasks.push(saver);
        #[cfg(feature = "automation")]
        if let Some(requests) = crate::automation::listen() {
            let serve = cx.spawn_in(window, async move |this, cx| {
                while let Ok(request) = requests.recv().await {
                    // Window-level commands run outside the workspace's own
                    // update, since drawing and input both re-enter it.
                    let command = request.command.clone();
                    let window_level = matches!(
                        command["cmd"].as_str(),
                        Some(
                            "screenshot"
                                | "click"
                                | "drag"
                                | "key"
                                | "probe"
                                | "quads"
                                | "clipped"
                                | "count_color"
                                | "frame"
                                | "math"
                        )
                    );
                    let started = std::time::Instant::now();
                    let mut reply = if window_level {
                        cx.update(|window, cx| window_command(&command, window, cx))
                            .unwrap_or(serde_json::Value::Null)
                    } else {
                        this.update_in(cx, |this, window, cx| {
                            this.automate(&request.command, window, cx)
                        })
                        .unwrap_or(serde_json::Value::Null)
                    };
                    // `timed`: what the action costs the user, measured as
                    // the action plus laying out and painting the frame
                    // that shows it (everything but the GPU's share).
                    if command["timed"].as_bool() == Some(true) {
                        _ = cx.update(|window, cx| window.draw(cx).clear(cx));
                        let ms = started.elapsed().as_secs_f64() * 1000.;
                        if let Some(reply) = reply.as_object_mut() {
                            reply.insert("ms".into(), ms.into());
                        }
                    }
                    let _ = request.reply.send(reply).await;
                }
            });
            this._tasks.push(serve);
        }
        this
    }

    #[cfg(feature = "automation")]
    fn automate(
        &mut self,
        command: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> serde_json::Value {
        use serde_json::json;
        match command["cmd"].as_str().unwrap_or_default() {
            "add_project" => {
                let path = PathBuf::from(command["path"].as_str().unwrap_or_default());
                return json!({ "project": self.adopt_project(&path, window, cx) });
            }
            "state" if self.active_view().is_none() => {
                return json!({
                    "chat": null,
                    "chats": self.slots.iter().map(|slot| slot.title(cx).to_string()).collect::<Vec<_>>(),
                    "sidebar": self.sidebar_outline(cx),
                    "text_size": f32::from(cx.theme().font_size),
                });
            }
            _ => {}
        }
        let Some(chat) = self.active_view() else {
            return json!({ "error": "no open chat" });
        };
        match command["cmd"].as_str().unwrap_or_default() {
            "state" => json!({
                "active": self.active,
                "chats": self.slots.iter().map(|slot| slot.title(cx).to_string()).collect::<Vec<_>>(),
                "panel_open": self.panel_open,
                "layout": { "sidebar": self.sidebar_width, "panel": self.panel_width },
                "text_size": f32::from(cx.theme().font_size),
                "sidebar_status": self.status_shown,
                "panel": self.panel.read(cx).snapshot(),
                "models": { "count": self.models.len(), "refreshing": self.refreshing_models },
                "dark": cx.theme().mode.is_dark(),
                "text_size_badge": self.text_size_badge.map(|(size, _)| size),
                "preparing": chat.read(cx).preparing.as_ref().map(|reason| reason.to_string()),
                "cwd": chat.read(cx).cwd,
                "sidebar": self.sidebar_outline(cx),
                "subagents": self.active_parent(cx).and_then(|p| self.families.get(&p)).map(|family| json!({
                    "collapsed": family.collapsed,
                    "children": family.children.iter().map(|child| json!({
                        "title": child.view.read(cx).title.as_ref(),
                        "running": child.running,
                        "hidden": child.hidden,
                        "viewing": self.active_child.as_ref() == Some(&child.view),
                        "chat": child.view.read(cx).snapshot(cx),
                    })).collect::<Vec<_>>(),
                })),
                "signed_in": self.life.signed_in(),
                "agent_running": self.life.chat(chat.read(cx).id).is_some_and(|c| c.agent.is_some()),
                "chat": chat.read(cx).snapshot(cx),
            }),
            "type" => {
                let text = command["text"].as_str().unwrap_or_default();
                chat.update(cx, |chat, cx| chat.set_draft(text, window, cx));
                json!({ "ok": true })
            }
            "submit" => {
                chat.update(cx, |chat, cx| chat.submit(window, cx));
                json!({ "ok": true })
            }
            // What the active thread's project offers first: a new thread
            // on its current branch.
            "new_chat" => {
                let project = self.slot_place(self.active, cx).project;
                match project {
                    Some(project) => {
                        self.new_thread(project, ThreadMode::CurrentBranch, window, cx)
                    }
                    None => self.new_chat(window, cx),
                }
                json!({ "ok": true })
            }
            // A model list from a file, read as a refresh reads OpenRouter's:
            // tests use a saved real one rather than the network.
            "load_models" => {
                let body = std::fs::read(command["path"].as_str().unwrap_or_default())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                match body.map(|body| crate::models::parse(&body)) {
                    Some(Ok(models)) => {
                        let count = models.len();
                        self.set_models(models, cx);
                        json!({ "models": count })
                    }
                    _ => json!({ "error": "no model list there" }),
                }
            }
            "refresh_models" => {
                self.refresh_models(cx);
                json!({ "ok": true })
            }
            "set_default_model" => {
                self.set_default_model(command["id"].as_str().unwrap_or_default().to_string(), cx);
                json!({ "ok": true })
            }
            // What clicking the model's name in the composer does.
            "open_model_picker" => {
                chat.update(cx, |chat, cx| chat.show_picker(true, window, cx));
                json!({ "ok": true })
            }
            "pick_model" => {
                let id = command["id"].as_str().unwrap_or_default().to_string();
                chat.update(cx, |chat, cx| chat.pick_model(id, cx));
                json!({ "ok": true })
            }
            "select_tab" => {
                let tab = match command["tab"].as_str() {
                    Some("changes") | None => crate::panel::Tab::Changes,
                    Some(id) => crate::panel::Tab::View(id.to_string()),
                };
                self.panel.update(cx, |panel, cx| panel.select(tab, cx));
                json!({ "ok": true })
            }
            "appearance" => {
                let choice = match command["choice"].as_str() {
                    Some("light") => Appearance::Light,
                    Some("dark") => Appearance::Dark,
                    _ => Appearance::System,
                };
                self.set_appearance(choice, window, cx);
                json!({ "ok": true })
            }
            "sign_out" => {
                self.sign_out(window, cx);
                json!({ "ok": true })
            }
            // Signs in with whatever credential is available without the
            // browser: the env key, or the replayed agent under SENSE_REPLAY.
            "sign_in" => {
                let account = load_account();
                self.account_loaded(account, window, cx);
                json!({ "ok": true })
            }
            // A click on a chat's sidebar row: opens it, or folds its
            // subagents if it's already open.
            "click_chat" => {
                let ix = command["index"].as_u64().unwrap_or(0) as usize;
                if ix >= self.slots.len() {
                    return json!({ "error": "no such chat" });
                }
                self.click_chat(ix, window, cx);
                json!({ "ok": true })
            }
            "select_child" => {
                self.select_child(command["index"].as_u64().unwrap_or(0) as usize, window, cx);
                json!({ "ok": self.active_child.is_some() })
            }
            "hide_child" => {
                self.hide_child(command["index"].as_u64().unwrap_or(0) as usize, window, cx);
                json!({ "ok": true })
            }
            "set_folder" => {
                let id = chat.read(cx).id;
                let cwd = PathBuf::from(command["path"].as_str().unwrap_or_default());
                self.dispatch(Input::SetFolder { chat: id, cwd }, window, cx);
                json!({ "ok": true })
            }
            "new_thread" => {
                let Some(project) = self
                    .library
                    .projects
                    .get(command["project"].as_u64().unwrap_or(0) as usize)
                else {
                    return json!({ "error": "no such project" });
                };
                let mode = match command["mode"].as_str() {
                    Some("worktree") => ThreadMode::Worktree,
                    _ => ThreadMode::CurrentBranch,
                };
                self.new_thread(project.id, mode, window, cx);
                json!({ "ok": true })
            }
            "new_folder" => {
                let Some(project) = self
                    .library
                    .projects
                    .get(command["project"].as_u64().unwrap_or(0) as usize)
                else {
                    return json!({ "error": "no such project" });
                };
                self.new_folder(project.id, window, cx);
                json!({ "ok": true })
            }
            "rename_folder" => {
                let Some(folder) = self
                    .library
                    .folders
                    .get(command["folder"].as_u64().unwrap_or(0) as usize)
                else {
                    return json!({ "error": "no such folder" });
                };
                self.library
                    .rename_folder(folder.id, command["name"].as_str().unwrap_or_default());
                self.renaming = None;
                self.save_library(cx);
                json!({ "ok": true })
            }
            "delete_folder" => {
                let Some(folder) = self
                    .library
                    .folders
                    .get(command["folder"].as_u64().unwrap_or(0) as usize)
                else {
                    return json!({ "error": "no such folder" });
                };
                self.delete_folder(folder.id, cx);
                json!({ "ok": true })
            }
            // What dropping the sidebar thread at `index` on a folder (or a
            // project's header, with no folder) does.
            "drop_thread" => {
                let ix = command["index"].as_u64().unwrap_or(0) as usize;
                let project = self
                    .library
                    .projects
                    .get(command["project"].as_u64().unwrap_or(0) as usize)
                    .map(|p| p.id);
                let folder = command["folder"]
                    .as_u64()
                    .and_then(|f| self.library.folders.get(f as usize))
                    .map(|f| f.id);
                let (Some(project), true) = (project, ix < self.slots.len()) else {
                    return json!({ "error": "no such thread or project" });
                };
                json!({ "moved": self.drop_thread(ix, project, folder, cx) })
            }
            "attach_image" => {
                let path = PathBuf::from(command["path"].as_str().unwrap_or_default());
                chat.update(cx, |chat, cx| chat.attach_files(vec![path], cx));
                json!({ "ok": true })
            }
            // A clipboard holding an image file's bytes, pasted. Built here
            // so the user's real clipboard is never touched.
            "paste_image" => {
                let path = command["path"].as_str().unwrap_or_default();
                let Ok(bytes) = std::fs::read(path) else {
                    return json!({ "error": "unreadable image" });
                };
                let item = ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes));
                let consumed = chat.update(cx, |chat, cx| chat.paste_item(&item, cx));
                json!({ "consumed": consumed })
            }
            "paste_text" => {
                let item =
                    ClipboardItem::new_string(command["text"].as_str().unwrap_or_default().into());
                let consumed = chat.update(cx, |chat, cx| chat.paste_item(&item, cx));
                json!({ "consumed": consumed })
            }
            "toggle_fold" => {
                let fold = command["index"].as_u64().unwrap_or(0) as usize;
                chat.update(cx, |chat, cx| chat.toggle_fold(fold, cx));
                json!({ "ok": true })
            }
            "stop" => {
                let id = chat.read(cx).id;
                self.dispatch(Input::Stop { chat: id }, window, cx);
                json!({ "ok": true })
            }
            "delete_chat" => {
                let ix = command["index"].as_u64().unwrap_or(0) as usize;
                if ix >= self.slots.len() {
                    return json!({ "error": "no such chat" });
                }
                self.delete_chat(ix, window, cx);
                json!({ "ok": true })
            }
            "select_chat" => {
                let ix = command["index"].as_u64().unwrap_or(0) as usize;
                if ix >= self.slots.len() {
                    return json!({ "error": "no such chat" });
                }
                self.select(ix, window, cx);
                json!({ "ok": true })
            }
            "toggle_panel" => {
                self.panel_open = !self.panel_open;
                cx.notify();
                json!({ "ok": self.panel_open })
            }
            "answer_permission" => {
                let choice = command["choice"].as_u64().map(|choice| choice as usize);
                let answered = chat.update(cx, |chat, cx| chat.answer_latest(choice, cx));
                json!({ "ok": answered })
            }
            other => json!({ "error": format!("unknown command {other:?}") }),
        }
    }

    fn account_loaded(
        &mut self,
        account: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match account {
            _ if self.replaying => {
                self.account = Account::SignedIn { from_env: true };
                let credential = "replay".to_string();
                self.dispatch(Input::SignedIn { credential }, window, cx);
            }
            Some((credential, from_env)) => {
                self.account = Account::SignedIn { from_env };
                self.start_librarian(&credential, window, cx);
                self.dispatch(Input::SignedIn { credential }, window, cx);
            }
            None => {
                self.account = Account::SignedOut;
                self.dispatch(Input::SignedOut, window, cx);
            }
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Warms the open chat's agent so its startup overlaps the user typing.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active_view() {
            let chat = view.read(cx).id;
            self.dispatch(Input::Focus { chat }, window, cx);
        }
    }

    /// The librarian is an fx process outside any chat, for reading the
    /// session store: listing past sessions and paging their history. It
    /// never holds an active session, so it can't disturb a chat's.
    fn start_librarian(&mut self, credential: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.librarian.take() {
            old.shutdown();
        }
        let Some(mut launch) =
            agent_launch::fx_launch(credential, &self.default_model(), &self.models)
        else {
            return;
        };
        // Recordings are of one chat's conversation, not the librarian's.
        if let sense_agent::Launch::Process { record, .. } = &mut launch {
            *record = None;
        }
        let librarian = sense_agent::spawn(launch);
        librarian.send(sense_agent::Command::ListSessions { cursor: None });
        let events = librarian.events.clone();
        self.librarian = Some(librarian);
        // Whatever the last librarian was asked won't be answered: ask this
        // one, for every chat still waiting on a page or retrying one.
        self.history_requested.clear();
        let waiting: Vec<(String, Option<String>)> = self
            .slots
            .iter()
            .filter_map(Slot::view)
            .filter_map(|view| {
                let session = view.read(cx).session.clone()?;
                let cursor = view.update(cx, |view, _| view.history_to_retry())?;
                Some((session.to_string(), cursor))
            })
            .collect();
        for (session, cursor) in waiting {
            self.request_history(&session, cursor);
        }
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let alive = this.update(cx, |this, cx| match event {
                    Event::SessionsListed { sessions, .. } => this.add_past_sessions(sessions, cx),
                    Event::History {
                        session,
                        turns,
                        older,
                    } => this.history_arrived(session.0.to_string(), turns, older, cx),
                    Event::HistoryFailed { session, error } => {
                        this.history_requested.remove(session.0.as_ref());
                        if let Some(view) = this.view_for_session(session.0.as_ref(), cx) {
                            view.update(cx, |view, cx| view.history_failed(&error, cx));
                        }
                    }
                    _ => {}
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        self._tasks.push(pump);
    }

    /// Fetches a page of a past session's transcript through the librarian.
    fn request_history(&mut self, session: &str, cursor: Option<String>) {
        if let Some(librarian) = &self.librarian {
            // SENSE_HISTORY_PAGE lets tests force small pages.
            let limit = std::env::var("SENSE_HISTORY_PAGE")
                .ok()
                .and_then(|limit| limit.parse().ok())
                .unwrap_or(HISTORY_PAGE);
            librarian.send(sense_agent::Command::History {
                session: SessionId::new(session.to_string()),
                cursor,
                limit,
            });
            self.history_requested.insert(session.to_string());
        }
    }

    /// Hovering a past session starts fetching its newest turns, so opening
    /// it shows them at once.
    fn preload(&mut self, ix: usize) {
        if let Some(Slot::Past(past)) = self.slots.get(ix) {
            let session = past.session.clone();
            if !self.history_cache.contains_key(&session)
                && !self.history_requested.contains(&session)
            {
                self.request_history(&session, None);
            }
        }
    }

    fn history_arrived(
        &mut self,
        session: String,
        turns: Vec<Vec<SessionUpdate>>,
        older: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.history_requested.remove(&session);
        let waiting = self
            .view_for_session(&session, cx)
            .filter(|view| matches!(view.read(cx).history, crate::chat::History::Loading { .. }));
        match waiting {
            Some(view) => view.update(cx, |view, cx| view.add_history_page(turns, older, cx)),
            // Fetched ahead of opening (a hover): kept until the chat opens,
            // if it's still in the sidebar to be opened.
            None if self
                .slots
                .iter()
                .any(|slot| matches!(slot, Slot::Past(past) if past.session == session)) =>
            {
                self.history_cache.insert(session, (turns, older));
            }
            None => {}
        }
    }

    /// A chat opened from a past session shows its newest turns: from the
    /// cache if a hover already fetched them, otherwise as they arrive.
    fn show_history(&mut self, view: &Entity<ChatView>, session: &str, cx: &mut Context<Self>) {
        view.update(cx, |view, _| view.history = crate::chat::History::NotLoaded);
        let first = view.update(cx, |view, _| view.next_history_page(true));
        if first.is_none() {
            return;
        }
        match self.history_cache.remove(session) {
            Some((turns, older)) => {
                view.update(cx, |view, cx| view.add_history_page(turns, older, cx))
            }
            None if self.history_requested.contains(session) => {}
            None => self.request_history(session, None),
        }
    }

    /// Lists past sessions under the open chats, newest first, skipping any
    /// already shown.
    fn add_past_sessions(&mut self, sessions: Vec<SessionInfo>, cx: &mut Context<Self>) {
        for info in sessions {
            let session = info.session_id.0.to_string();
            let shown = self.life.chat_for(&session).is_some()
                || self
                    .slots
                    .iter()
                    .any(|slot| matches!(slot, Slot::Past(past) if past.session == session));
            if shown {
                continue;
            }
            let title = info
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| "Untitled".into());
            self.slots.push(Slot::Past(PastSession {
                session,
                cwd: info.cwd,
                title: title.into(),
            }));
        }
        cx.notify();
    }

    /// Runs one input through the lifecycle, carries out its effects, then
    /// brings every chat view in line with the new state.
    fn dispatch(&mut self, input: Input, window: &mut Window, cx: &mut Context<Self>) {
        for effect in self.life.step(input) {
            self.execute(effect, window, cx);
        }
        for chat in self.views().cloned().collect::<Vec<_>>() {
            let id = chat.read(cx).id;
            if let Some(state) = self.life.chat(id) {
                chat.update(cx, |chat, cx| chat.sync(state, cx));
            }
        }
        cx.notify();
    }

    fn execute(&mut self, effect: Effect, window: &mut Window, cx: &mut Context<Self>) {
        match effect {
            Effect::StartAgent {
                agent,
                chat,
                credential,
            } => {
                let launch = if self.replaying {
                    agent_launch::replay_launch()
                } else {
                    // Starts on the chat's model, so its first session
                    // needs no switch.
                    let model = self
                        .view(chat, cx)
                        .map(|view| view.read(cx).model.clone())
                        .unwrap_or_else(|| self.default_model());
                    agent_launch::fx_launch(&credential, &model, &self.models)
                };
                let Some(launch) = launch else {
                    self.notify_active("fx was not found. Build vendor/fx or set SENSE_FX.", cx);
                    self.dispatch(Input::AgentExited { agent }, window, cx);
                    return;
                };
                let handle = sense_agent::spawn(launch);
                let events = handle.events.clone();
                self.agents.insert(agent, handle);
                let pump = cx.spawn_in(window, async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        let delivered = this.update_in(cx, |this, window, cx| {
                            this.on_agent_event(agent, event, window, cx)
                        });
                        if delivered.is_err() {
                            break;
                        }
                    }
                });
                self._tasks.push(pump);
            }
            Effect::StopAgent { agent } => {
                if let Some(handle) = self.agents.remove(&agent) {
                    handle.shutdown();
                }
            }
            Effect::Send { agent, command } => {
                if let Some(handle) = self.agents.get(&agent) {
                    handle.send(match command {
                        Command::NewSession { chat, cwd } => {
                            sense_agent::Command::NewSession { cwd, tag: chat }
                        }
                        Command::Prompt {
                            session,
                            cwd,
                            prompt,
                        } => sense_agent::Command::Prompt {
                            session: SessionId::new(session),
                            cwd,
                            text: prompt.text,
                            model: prompt.model,
                            images: prompt
                                .images
                                .into_iter()
                                .map(|image| (image.mime, image.bytes))
                                .collect(),
                        },
                        Command::Cancel { session } => sense_agent::Command::Cancel {
                            session: SessionId::new(session),
                        },
                    });
                }
            }
            Effect::TakeBaseline { chat, cwd } => {
                if let Some(view) = self.view(chat, cx) {
                    view.update(cx, |view, cx| view.take_baseline(cwd, cx));
                }
            }
            Effect::RefreshDiff { chat } => {
                if let Some(view) = self.view(chat, cx) {
                    view.update(cx, |view, cx| view.refresh_patch(cx));
                }
            }
            Effect::ForgetChat { chat, session } => self.forget_chat(chat, session, window, cx),
            Effect::Notify { chat, message } => {
                if let Some(view) = self.view(chat, cx) {
                    let text = notice_text(&message);
                    // What ended the turn, for the thread's status.
                    let failed = match &message {
                        Notice::TurnFailed(error) => Some(error.clone().into()),
                        Notice::AgentStopped => Some("fx stopped".into()),
                        _ => None,
                    };
                    view.update(cx, |view, cx| {
                        if failed.is_some() {
                            view.turn_failed = failed;
                        }
                        view.notice(text, cx)
                    });
                }
            }
        }
    }

    fn on_agent_event(
        &mut self,
        agent: AgentId,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.life.chat_of_agent(agent).is_some();
        match event {
            Event::Ready { .. } => {}
            Event::SessionCreated { tag, session } => {
                let session = session.0.to_string();
                if let Some(place) = self.pending_place.remove(&tag)
                    && place.project.is_some()
                {
                    self.library.place(&session, place);
                    self.save_library(cx);
                }
                self.dispatch(
                    Input::SessionCreated {
                        agent,
                        chat: tag,
                        session,
                    },
                    window,
                    cx,
                )
            }
            Event::SessionFailed { tag, error } => self.dispatch(
                Input::SessionFailed {
                    agent,
                    chat: tag,
                    error,
                },
                window,
                cx,
            ),
            // Transcript updates bypass the lifecycle, so the staleness
            // check it does for everything else happens here.
            Event::Update {
                session,
                update: SessionUpdate::SubagentUpdate(subagent),
            } if current => {
                let Some(parent) = self.life.chat_for(&session.0) else {
                    return;
                };
                self.subagent_update(parent, subagent, window, cx);
            }
            Event::Update { session, update } if current && self.child_of(&session.0).is_some() => {
                let (parent, ix) = self.child_of(&session.0).unwrap();
                let view = self.families[&parent].children[ix].view.clone();
                view.update(cx, |view, cx| view.apply(update, cx));
            }
            Event::Update { session, update } if current => {
                let finished = matches!(
                    &update,
                    SessionUpdate::ToolCallUpdate(update)
                        if matches!(update.fields.status, Some(ToolCallStatus::Completed | ToolCallStatus::Failed))
                );
                if let Some(view) = self.view_for_session(&session.0, cx) {
                    view.update(cx, |view, cx| view.apply(update, cx));
                }
                if finished {
                    let session = session.0.to_string();
                    self.dispatch(Input::ToolFinished { agent, session }, window, cx);
                }
            }
            Event::Update { .. } => {}
            // A subagent's request shows in its own transcript.
            Event::Permission(ask) => match self
                .view_for_session(&ask.session.0, cx)
                .or_else(|| {
                    self.child_of(&ask.session.0)
                        .map(|(parent, ix)| self.families[&parent].children[ix].view.clone())
                })
                .filter(|_| current)
            {
                Some(view) => view.update(cx, |view, cx| view.ask_permission(ask, cx)),
                None => ask.answer(None),
            },
            Event::TurnEnded { session, error, .. } => self.dispatch(
                Input::TurnEnded {
                    agent,
                    session: session.0.to_string(),
                    error,
                },
                window,
                cx,
            ),
            // Only the librarian lists sessions.
            Event::SessionsListed { .. } | Event::History { .. } | Event::HistoryFailed { .. } => {}
            Event::Error(message) if current => self.notify_active(&message, cx),
            Event::Error(_) => {}
            Event::Exited => {
                self.agents.remove(&agent);
                self.dispatch(Input::AgentExited { agent }, window, cx);
            }
        }
    }

    /// A subagent appeared or changed. The first update for a session
    /// announces it, before any of its own traffic.
    fn subagent_update(
        &mut self,
        parent: ChatId,
        subagent: sense_agent::acp::SubagentUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = subagent.session_id.0.to_string();
        let running = match subagent.state.value() {
            Some(sense_agent::acp::StateUpdate::Running(_)) => Some(true),
            Some(_) => Some(false),
            None => None,
        };
        let title = subagent.title.value().cloned();
        let family = self.families.entry(parent).or_default();
        let ix = match family
            .children
            .iter()
            .position(|child| child.session == session)
        {
            Some(ix) => ix,
            None => {
                let cwd = self
                    .life
                    .chat(parent)
                    .map(|chat| chat.cwd.clone())
                    .unwrap_or_default();
                let id = self.next_chat;
                self.next_chat += 1;
                let view = cx.new(|cx| {
                    let mut view = ChatView::new(id, cwd, window, cx);
                    view.read_only = true;
                    view
                });
                cx.subscribe(&view, |this, view, event: &ChatEvent, cx| {
                    if let ChatEvent::PatchChanged = event
                        && this.active_view().as_ref() == Some(&view)
                    {
                        this.show_patch_of(&view, cx);
                    }
                    cx.notify();
                })
                .detach();
                let family = self.families.get_mut(&parent).unwrap();
                family.children.push(Child {
                    session,
                    view,
                    running: true,
                    hidden: false,
                });
                family.children.len() - 1
            }
        };
        let child = &mut self.families.get_mut(&parent).unwrap().children[ix];
        if let Some(title) = title {
            child.view.update(cx, |view, _| view.title = title.into());
        }
        if let Some(running) = running {
            child.running = running;
            child
                .view
                .update(cx, |view, cx| view.set_running(running, cx));
        }
        cx.notify();
    }

    fn child_of(&self, session: &str) -> Option<(ChatId, usize)> {
        self.families.iter().find_map(|(parent, family)| {
            family
                .children
                .iter()
                .position(|child| child.session == session)
                .map(|ix| (*parent, ix))
        })
    }

    /// The chat whose sidebar row is selected (a subagent's parent, when a
    /// subagent is being viewed).
    fn active_parent(&self, cx: &App) -> Option<ChatId> {
        self.slots
            .get(self.active)
            .and_then(Slot::view)
            .map(|view| view.read(cx).id)
    }

    fn select_child(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let child = self
            .active_parent(cx)
            .and_then(|parent| self.families.get(&parent))
            .and_then(|family| family.children.get(ix))
            .map(|child| child.view.clone());
        let Some(child) = child else {
            return;
        };
        self.active_child = Some(child);
        if let Some(view) = self.active_view() {
            view.read(cx).focus_handle(cx).focus(window, cx);
            self.show_patch_of(&view, cx);
        }
        cx.notify();
    }

    fn hide_child(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.active_parent(cx) else {
            return;
        };
        let Some(child) = self
            .families
            .get_mut(&parent)
            .and_then(|f| f.children.get_mut(ix))
        else {
            return;
        };
        child.hidden = true;
        if self.active_child.as_ref() == Some(&child.view) {
            let active = self.active;
            self.select(active, window, cx);
        }
        cx.notify();
    }

    /// The first click on a chat opens it; clicking the open chat again
    /// folds its subagents away or back.
    fn click_chat(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self
            .slots
            .get(ix)
            .and_then(Slot::view)
            .map(|view| view.read(cx).id);
        let reopening = ix == self.active && self.active_child.is_none();
        match parent.and_then(|parent| self.families.get_mut(&parent)) {
            Some(family) if reopening && family.children.iter().any(|c| !c.hidden) => {
                family.collapsed = !family.collapsed;
                cx.notify();
            }
            _ => self.select(ix, window, cx),
        }
    }

    fn view(&self, chat: ChatId, cx: &App) -> Option<Entity<ChatView>> {
        self.views().find(|view| view.read(cx).id == chat).cloned()
    }

    fn view_for_session(&self, session: &str, cx: &App) -> Option<Entity<ChatView>> {
        self.life
            .chat_for(session)
            .and_then(|chat| self.view(chat, cx))
    }

    fn notify_active(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(view) = self.active_view() {
            view.update(cx, |view, cx| view.notice(message.to_string(), cx));
        }
    }

    /// Opens a new thread where the user last worked: the last project used
    /// (or, for automated runs, SENSE_CWD's), on its current branch. With
    /// no project yet there is none; the window offers to add one.
    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = match std::env::var_os("SENSE_CWD") {
            Some(cwd) => Some(self.add_project(std::path::Path::new(&cwd), cx)),
            None => self
                .library
                .last_project
                .filter(|p| self.library.project(*p).is_some())
                .or_else(|| self.library.projects.first().map(|p| p.id)),
        };
        if let Some(project) = project {
            self.new_thread(project, ThreadMode::CurrentBranch, window, cx);
        } else {
            cx.notify();
        }
    }

    /// Opens a new thread working in `cwd`, shown at `place` in the sidebar.
    fn new_chat_in(
        &mut self,
        cwd: PathBuf,
        place: Place,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ChatId {
        let id = self.next_chat;
        self.next_chat += 1;
        let view = self.make_view(id, cwd.clone(), window, cx);
        self.slots.insert(0, Slot::Live(view));
        // Saved under the session id once fx gives it one.
        self.pending_place.insert(id, place);
        self.dispatch(Input::NewChat { chat: id, cwd }, window, cx);
        self.select(0, window, cx);
        id
    }

    /// A new thread in a project: in the repository's own checkout, or in a
    /// fresh worktree branched from origin/main. Sending waits for the
    /// worktree, so nothing can run in the wrong folder.
    fn new_thread(
        &mut self,
        project: ProjectId,
        mode: ThreadMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.library.project(project).cloned() else {
            return;
        };
        if self.library.last_project != Some(project.id) {
            self.library.last_project = Some(project.id);
            self.save_library(cx);
        }
        let place = Place {
            project: Some(project.id),
            folder: None,
        };
        let id = self.new_chat_in(project.path.clone(), place, window, cx);
        if mode == ThreadMode::CurrentBranch {
            return;
        }
        let Some(view) = self.view(id, cx) else {
            return;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let path = crate::worktree::root(&project.name).join(format!("thread-{stamp}"));
        let branch = crate::worktree::branch_name("thread", stamp);
        view.update(cx, |view, cx| {
            view.preparing = Some("Creating worktree…".into());
            view.worktree = Some((path.clone(), branch.clone().into()));
            cx.notify();
        });
        let repo = project.path.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let target = path.clone();
            let made = cx
                .background_spawn(async move { crate::worktree::create(&repo, &target, &branch) })
                .await;
            _ = this.update_in(cx, |this, window, cx| {
                let Some(view) = this.view(id, cx) else {
                    // Deleted while the worktree was being made: it would
                    // be left behind with nothing pointing at it.
                    if made.is_ok() {
                        remove_worktree(path, cx);
                    }
                    return;
                };
                match made {
                    Ok(()) => {
                        this.dispatch(
                            Input::SetFolder {
                                chat: id,
                                cwd: path,
                            },
                            window,
                            cx,
                        );
                        view.update(cx, |view, cx| {
                            view.preparing = None;
                            cx.notify();
                        });
                    }
                    Err(err) => view.update(cx, |view, cx| {
                        view.preparing = Some("No worktree".into());
                        view.notice(format!("Couldn't create the worktree: {err:#}"), cx);
                    }),
                }
            });
        });
        self._tasks.push(task);
    }

    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = picked.await
                && let Some(path) = paths.pop()
            {
                _ = this.update_in(cx, |this, window, cx| this.adopt_project(&path, window, cx));
            }
        });
        self._tasks.push(task);
    }

    fn set_appearance(&mut self, choice: Appearance, window: &mut Window, cx: &mut Context<Self>) {
        self.library.appearance = choice;
        self.save_library(cx);
        self.apply_appearance(window, cx);
    }

    /// Light or dark, as chosen or as the system is now.
    fn apply_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::ThemeMode;
        let mode = match self.library.appearance {
            Appearance::System => window.appearance().into(),
            Appearance::Light => ThemeMode::Light,
            Appearance::Dark => ThemeMode::Dark,
        };
        if cx.theme().mode != mode {
            crate::apply_mode(mode, cx);
        }
    }

    fn hover_thread(&mut self, hovered: bool, row: (usize, Option<usize>), cx: &mut Context<Self>) {
        if hovered {
            self.hovered = Some(row);
        } else if self.hovered == Some(row) {
            self.hovered = None;
        }
        cx.notify();
    }

    /// The hovered row's chat: a thread's own, or its subagent's.
    fn hovered_view(&self, cx: &App) -> Option<Entity<ChatView>> {
        let (ix, child) = self.hovered?;
        let thread = self.slots.get(ix)?.view()?;
        match child {
            None => Some(thread.clone()),
            Some(cix) => {
                let family = self.families.get(&thread.read(cx).id)?;
                Some(family.children.get(cix)?.view.clone())
            }
        }
    }

    /// What the hovered thread is doing, top right in the sidebar beside
    /// the traffic lights.
    fn hovered_status(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use crate::chat::Status;
        self.status_shown = None;
        let (label, color) = {
            let status = self.hovered_view(cx)?.read(cx).status();
            let theme = cx.theme();
            let ago = |at: std::time::Instant| elapsed(at.elapsed());
            match status {
                Status::Idle => ("Idle".to_string(), theme.muted_foreground),
                Status::Preparing(reason) => (reason.to_string(), theme.muted_foreground),
                Status::Working { since } => (
                    format!("Working · {}", elapsed(since.elapsed())),
                    theme.info,
                ),
                Status::NeedsPermission(title) => {
                    (format!("Needs permission · {title}"), theme.warning)
                }
                Status::Done { at } => (format!("Done · {} ago", ago(at)), theme.success),
                Status::Stopped { at } => {
                    (format!("Stopped · {} ago", ago(at)), theme.muted_foreground)
                }
                Status::Failed { error, .. } => (format!("Failed · {error}"), theme.danger),
            }
        };
        // Times shown move on by the second.
        if self.status_tick.is_none() {
            self.status_tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                _ = this.update(cx, |this, cx| {
                    this.status_tick = None;
                    if this.hovered.is_some() {
                        cx.notify();
                    }
                });
            }));
        }
        self.status_shown = Some(label.clone());
        Some(
            h_flex()
                .absolute()
                .top(px(7.))
                .right(px(10.))
                // Clear of the traffic lights.
                .left(px(78.))
                .h(px(22.))
                .justify_end()
                .gap_1p5()
                .text_xs()
                .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(color))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .into_any_element(),
        )
    }

    fn default_model(&self) -> String {
        self.library
            .model
            .clone()
            .unwrap_or_else(|| agent_launch::DEFAULT_MODEL.to_string())
    }

    fn show_default_picker(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.default_picker_open = open;
        if open {
            let (models, current) = (self.models.clone(), self.default_model());
            self.default_picker
                .update(cx, |picker, cx| picker.open(models, current, window, cx));
        } else if let Some(chat) = self.active_view() {
            chat.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn set_default_model(&mut self, id: String, cx: &mut Context<Self>) {
        self.library.model = Some(id.clone());
        self.save_library(cx);
        for view in self.views().cloned().collect::<Vec<_>>() {
            view.update(cx, |view, cx| view.follow_default(&id, cx));
        }
        cx.notify();
    }

    /// Fetches OpenRouter's model list in the background; the app goes on
    /// as it is meanwhile.
    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        if self.refreshing_models {
            return;
        }
        self.refreshing_models = true;
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let fetched = cx.background_spawn(async { crate::models::fetch() }).await;
            _ = this.update(cx, |this, cx| {
                this.refreshing_models = false;
                match fetched {
                    Ok(models) => this.set_models(models, cx),
                    Err(err) => {
                        this.notify_active(&format!("Couldn't refresh models: {err:#}"), cx)
                    }
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    /// A refreshed list: kept for next time, and given to every picker.
    fn set_models(&mut self, models: Vec<crate::models::Model>, cx: &mut Context<Self>) {
        self.library.models = models.clone();
        self.save_library(cx);
        self.share_models(models, cx);
    }

    fn share_models(&mut self, models: Vec<crate::models::Model>, cx: &mut Context<Self>) {
        self.models = std::sync::Arc::new(models);
        cx.notify();
        for view in self.views().cloned().collect::<Vec<_>>() {
            let models = self.models.clone();
            view.update(cx, |view, cx| {
                view.models = models;
                cx.notify();
            });
        }
    }

    /// A project the user added: with no thread open, one starts in it.
    fn adopt_project(
        &mut self,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ProjectId {
        let project = self.add_project(path, cx);
        if self.active_view().is_none() {
            self.new_thread(project, ThreadMode::CurrentBranch, window, cx);
        }
        project
    }

    fn add_project(&mut self, path: &std::path::Path, cx: &mut Context<Self>) -> ProjectId {
        let id = self.library.add_project(path);
        self.save_library(cx);
        cx.notify();
        id
    }

    fn new_folder(&mut self, project: ProjectId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(folder) = self.library.add_folder(project, "New folder") {
            self.folded_projects.remove(&project);
            self.save_library(cx);
            self.start_rename(folder, window, cx);
        }
    }

    /// Renames a folder in place: Enter or leaving the field keeps the name.
    fn start_rename(&mut self, folder: FolderId, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .library
            .folder(folder)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let name = input.read(cx).value().to_string();
                    this.library.rename_folder(folder, &name);
                    this.renaming = None;
                    this.save_library(cx);
                    cx.notify();
                }
            },
        )
        .detach();
        // Focused with the whole name selected, so typing replaces it.
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some((folder, input));
        cx.notify();
    }

    fn delete_folder(&mut self, folder: FolderId, cx: &mut Context<Self>) {
        self.library.delete_folder(folder);
        for place in self.pending_place.values_mut() {
            if place.folder == Some(folder) {
                place.folder = None;
            }
        }
        self.save_library(cx);
        cx.notify();
    }

    /// A thread dropped on a folder (or a project's header): it moves there
    /// if that's in its own project. A thread's project is its repository.
    /// The sidebar row a thread is in now.
    fn slot_of(&self, thread: &ThreadKey, cx: &App) -> Option<usize> {
        self.slots.iter().position(|slot| match (slot, thread) {
            (Slot::Live(view), ThreadKey::Chat(chat)) => view.read(cx).id == *chat,
            (Slot::Past(past), ThreadKey::Past(session)) => past.session == *session,
            _ => false,
        })
    }

    fn drop_thread(
        &mut self,
        ix: usize,
        project: ProjectId,
        folder: Option<FolderId>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.slot_place(ix, cx).project != Some(project) {
            return false;
        }
        let moved = match &self.slots[ix] {
            Slot::Live(view) => {
                let chat = view.read(cx).id;
                match self.life.chat(chat).and_then(|c| c.session.clone()) {
                    Some(session) => {
                        let cwd = view.read(cx).cwd.clone();
                        self.library.move_thread(&session, folder, &cwd)
                    }
                    None => {
                        self.pending_place.insert(
                            chat,
                            Place {
                                project: Some(project),
                                folder,
                            },
                        );
                        true
                    }
                }
            }
            Slot::Past(past) => self.library.move_thread(&past.session, folder, &past.cwd),
        };
        if moved {
            self.save_library(cx);
            cx.notify();
        }
        moved
    }

    /// The sidebar as data: projects, their folders and threads, and the
    /// chats outside any project, by title.
    #[cfg(feature = "automation")]
    fn sidebar_outline(&self, cx: &App) -> serde_json::Value {
        use serde_json::json;
        let places: Vec<Place> = (0..self.slots.len())
            .map(|ix| self.slot_place(ix, cx))
            .collect();
        let titles = |keep: &dyn Fn(&Place) -> bool| -> Vec<String> {
            (0..self.slots.len())
                .filter(|ix| keep(&places[*ix]))
                .map(|ix| self.slots[ix].title(cx).to_string())
                .collect()
        };
        json!({
            "projects": self.library.projects.iter().map(|project| json!({
                "name": project.name,
                "folders": self.library.folders.iter().filter(|f| f.project == project.id).map(|folder| json!({
                    "name": folder.name,
                    "threads": titles(&|place| place.folder == Some(folder.id)),
                })).collect::<Vec<_>>(),
                "threads": titles(&|place| place.project == Some(project.id) && place.folder.is_none()),
            })).collect::<Vec<_>>(),
            "chats": titles(&|place| place.project.is_none()),
        })
    }

    /// Where a sidebar thread sits.
    fn slot_place(&self, ix: usize, cx: &App) -> Place {
        match &self.slots[ix] {
            Slot::Live(view) => {
                let view = view.read(cx);
                match &view.session {
                    Some(session) if self.library.threads.contains_key(session) => {
                        self.library.place_of(session, &view.cwd)
                    }
                    _ => self
                        .pending_place
                        .get(&view.id)
                        .copied()
                        .filter(|place| place.project.is_some())
                        .unwrap_or_else(|| self.library.place_of("", &view.cwd)),
                }
            }
            Slot::Past(past) => self.library.place_of(&past.session, &past.cwd),
        }
    }

    /// Saves the library off the main thread. Not before it has loaded:
    /// that would overwrite the saved one with a partial copy.
    fn save_library(&mut self, _: &mut Context<Self>) {
        _ = self.library_saves.try_send(self.library.clone());
    }

    fn make_view(
        &mut self,
        id: ChatId,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ChatView> {
        let (model, models) = (self.default_model(), self.models.clone());
        let view = cx.new(|cx| {
            let mut view = ChatView::new(id, cwd, window, cx);
            view.model = model;
            view.models = models;
            view
        });
        cx.subscribe_in(
            &view,
            window,
            move |this, view, event: &ChatEvent, window, cx| {
                match event {
                    ChatEvent::Submit(prompt) => {
                        let prompt = prompt.clone();
                        this.dispatch(Input::Submit { chat: id, prompt }, window, cx);
                    }
                    ChatEvent::Stop => this.dispatch(Input::Stop { chat: id }, window, cx),
                    ChatEvent::BaselineTaken(error) => {
                        let error = error.clone();
                        this.dispatch(Input::BaselineTaken { chat: id, error }, window, cx);
                    }
                    ChatEvent::PatchChanged => {
                        if this.active_view().as_ref() == Some(view) {
                            this.show_patch_of(view, cx);
                        }
                    }
                    ChatEvent::ViewShown(shown) => {
                        if this.active_view().as_ref() == Some(view) {
                            let views = view.read(cx).views.clone();
                            this.panel
                                .update(cx, |panel, cx| panel.show_views(&views, Some(shown), cx));
                            this.panel_open = true;
                        }
                    }
                    ChatEvent::TitleChanged => {}
                    ChatEvent::NeedOlderHistory => {
                        let session = this.life.chat(id).and_then(|chat| chat.session.clone());
                        let cursor = view.update(cx, |view, _| view.next_history_page(false));
                        if let (Some(session), Some(cursor)) = (session, cursor) {
                            this.request_history(&session, cursor);
                        }
                    }
                }
                cx.notify();
            },
        )
        .detach();
        view
    }

    /// Turns a past session in the sidebar into a real chat: its view, and
    /// its place in the lifecycle. Nothing runs until it's used.
    fn materialize(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Slot::Past(past)) = self.slots.get(ix).cloned() else {
            return;
        };
        let id = self.next_chat;
        self.next_chat += 1;
        let view = self.make_view(id, past.cwd.clone(), window, cx);
        view.update(cx, |view, _| view.title = past.title.clone());
        self.slots[ix] = Slot::Live(view.clone());
        let (cwd, session) = (past.cwd, past.session);
        self.show_history(&view, &session, cx);
        self.dispatch(
            Input::RestoreChat {
                chat: id,
                cwd,
                session,
            },
            window,
            cx,
        );
    }

    fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.materialize(ix, window, cx);
        self.active = ix;
        self.active_child = None;
        let Some(chat) = self.active_view() else {
            return;
        };
        chat.read(cx).focus_handle(cx).focus(window, cx);
        self.show_patch_of(&chat, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn show_patch_of(&mut self, chat: &Entity<ChatView>, cx: &mut Context<Self>) {
        let files = chat.read(cx).patch_files.clone();
        let views = chat.read(cx).views.clone();
        self.panel.update(cx, |panel, cx| {
            panel.show_files(files, cx);
            panel.show_views(&views, None, cx);
        });
        self.panel_open = !self.panel.read(cx).is_empty();
    }

    fn delete_chat(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A past session becomes a chat first, so deleting it goes through
        // the lifecycle like any other.
        self.materialize(ix, window, cx);
        if let Some(Slot::Live(view)) = self.slots.get(ix) {
            let chat = view.read(cx).id;
            self.dispatch(Input::DeleteChat { chat }, window, cx);
        }
    }

    /// Removes a deleted chat's view and what it left on disk: its change
    /// snapshot and fx's saved session.
    fn forget_chat(
        &mut self,
        chat: ChatId,
        session: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .slots
            .iter()
            .position(|slot| slot.view().is_some_and(|view| view.read(cx).id == chat))
        else {
            return;
        };
        let Slot::Live(view) = self.slots.remove(ix) else {
            return;
        };
        self.families.remove(&chat);
        // The worktree the thread was made with; or, for a past session
        // reopened, the one it works in. One still being made is removed
        // when it's done.
        let worktree = match view.read(cx).worktree.clone() {
            Some((path, _)) => path.exists().then_some(path),
            None => Some(view.read(cx).cwd.clone())
                .filter(|cwd| crate::worktree::is_thread_worktree(cwd)),
        };
        if let Some(path) = worktree {
            remove_worktree(path, cx);
        }
        if let Some(session) = &session {
            // A page still on its way is dropped when it comes.
            self.history_cache.remove(session);
            self.history_requested.remove(session);
        }
        let mut doomed: Vec<PathBuf> = view.read(cx).snapshot_dir().into_iter().collect();
        // A replayed session id names a real recording's folder; keep it.
        if !self.replaying {
            doomed.extend(session.as_deref().and_then(fx_session_dir));
        }
        cx.background_spawn(async move {
            for dir in doomed {
                let _ = std::fs::remove_dir_all(dir);
            }
        })
        .detach();
        // The nearest thread still shown (past sessions outside every
        // project aren't), or else a new one.
        let near = if self.active > ix {
            self.active - 1
        } else {
            self.active
        };
        let shown = |ix: &usize| self.slot_place(*ix, cx).project.is_some();
        let next = (near..self.slots.len())
            .find(shown)
            .or_else(|| (0..near.min(self.slots.len())).rev().find(shown));
        match next {
            Some(next) => self.select(next, window, cx),
            None => self.new_chat(window, cx),
        }
    }

    fn active_view(&self) -> Option<Entity<ChatView>> {
        self.active_child
            .clone()
            .or_else(|| self.slots.get(self.active).and_then(Slot::view).cloned())
    }

    fn views(&self) -> impl Iterator<Item = &Entity<ChatView>> {
        self.slots.iter().filter_map(Slot::view)
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.account = Account::SigningIn;
        cx.notify();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async {
                    sense_auth::sign_in(|url| {
                        std::process::Command::new("open").arg(url).status()?;
                        Ok(())
                    })
                })
                .await;
            _ = this.update_in(cx, |this, window, cx| match result {
                Ok(account) => this.account_loaded(Some((account.api_key, false)), window, cx),
                Err(err) => {
                    this.account = Account::SignedOut;
                    this.notify_active(&format!("Sign-in failed: {err}"), cx);
                }
            });
        });
        self._tasks.push(task);
    }

    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = sense_auth::sign_out() {
            self.notify_active(&format!("Sign-out failed: {err}"), cx);
            return;
        }
        self.account = Account::SignedOut;
        if let Some(librarian) = self.librarian.take() {
            librarian.shutdown();
        }
        self.dispatch(Input::SignedOut, window, cx);
    }

    /// Chats, each followed by its visible subagents unless folded away.
    /// The sidebar's threads: projects with their folders, then chats that
    /// belong to no project.
    fn render_chat_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let places: Vec<Place> = (0..self.slots.len())
            .map(|ix| self.slot_place(ix, cx))
            .collect();
        let mut rows = v_flex().w_full().gap_0p5();
        let view = cx.entity();
        rows = rows.child(section_header(
            "Projects",
            Some(
                Button::new("add-project")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx)))
                    .into_any_element(),
            ),
            &theme,
        ));
        for project in &self.library.projects {
            let pid = project.id;
            let folded = self.folded_projects.contains(&pid);
            let menu_view = view.clone();
            rows = rows.child(
                h_flex()
                    .id(("project", pid as usize))
                    .w_full()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .text_sm()
                    .hover(|this| this.bg(theme.sidebar_accent))
                    .drag_over::<DraggedThread>(|style, _, _, cx| {
                        style.bg(cx.theme().sidebar_accent)
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedThread, _, cx| {
                        if let Some(ix) = this.slot_of(&dragged.thread, cx) {
                            _ = this.drop_thread(ix, pid, None, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.folded_projects.remove(&pid) {
                            this.folded_projects.insert(pid);
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if folded {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(project.name.clone()),
                    )
                    .child(
                        Button::new(("project-menu", pid as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .dropdown_menu(move |menu, _, _| {
                                let (a, b, c, d) = (
                                    menu_view.clone(),
                                    menu_view.clone(),
                                    menu_view.clone(),
                                    menu_view.clone(),
                                );
                                menu.item(
                                    PopupMenuItem::new("New thread · current branch").on_click(
                                        move |_, window, cx| {
                                            a.update(cx, |this, cx| {
                                                this.new_thread(
                                                    pid,
                                                    ThreadMode::CurrentBranch,
                                                    window,
                                                    cx,
                                                )
                                            })
                                        },
                                    ),
                                )
                                .item(
                                    PopupMenuItem::new("New thread · worktree from origin/main")
                                        .on_click(move |_, window, cx| {
                                            b.update(cx, |this, cx| {
                                                this.new_thread(
                                                    pid,
                                                    ThreadMode::Worktree,
                                                    window,
                                                    cx,
                                                )
                                            })
                                        }),
                                )
                                .separator()
                                .item(PopupMenuItem::new("New folder").on_click(
                                    move |_, window, cx| {
                                        c.update(cx, |this, cx| this.new_folder(pid, window, cx))
                                    },
                                ))
                                .separator()
                                .item(
                                    PopupMenuItem::new("Remove project").on_click(
                                        move |_, _, cx| {
                                            d.update(cx, |this, cx| {
                                                this.library.remove_project(pid);
                                                this.save_library(cx);
                                                cx.notify();
                                            })
                                        },
                                    ),
                                )
                            }),
                    ),
            );
            if folded {
                continue;
            }
            for folder in self.library.folders.iter().filter(|f| f.project == pid) {
                let fid = folder.id;
                let open = !self.folded_folders.contains(&fid);
                let menu_view = view.clone();
                let renaming = self
                    .renaming
                    .as_ref()
                    .filter(|(id, _)| *id == fid)
                    .map(|(_, input)| input.clone());
                rows = rows.child(
                    h_flex()
                        .id(("folder", fid as usize))
                        .ml(px(12.))
                        .gap_1p5()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.sidebar_accent))
                        .drag_over::<DraggedThread>(|style, _, _, cx| {
                            style.bg(cx.theme().sidebar_accent)
                        })
                        .on_drop(cx.listener(move |this, dragged: &DraggedThread, _, cx| {
                            if let Some(ix) = this.slot_of(&dragged.thread, cx) {
                                _ = this.drop_thread(ix, pid, Some(fid), cx);
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.folded_folders.remove(&fid) {
                                this.folded_folders.insert(fid);
                            }
                            cx.notify();
                        }))
                        .child(
                            Icon::new(if open {
                                IconName::FolderOpen
                            } else {
                                IconName::Folder
                            })
                            .xsmall(),
                        )
                        .child(match renaming {
                            Some(input) => div()
                                .flex_1()
                                .child(TextInput::new(&input).xsmall())
                                .into_any_element(),
                            None => div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(folder.name.clone())
                                .into_any_element(),
                        })
                        .context_menu(move |menu, _, _| {
                            let (a, b) = (menu_view.clone(), menu_view.clone());
                            menu.item(PopupMenuItem::new("Rename").on_click(
                                move |_, window, cx| {
                                    a.update(cx, |this, cx| this.start_rename(fid, window, cx))
                                },
                            ))
                            .item(
                                PopupMenuItem::new("Delete folder").on_click(move |_, _, cx| {
                                    b.update(cx, |this, cx| this.delete_folder(fid, cx))
                                }),
                            )
                        }),
                );
                if open {
                    for ix in (0..self.slots.len()).filter(|ix| places[*ix].folder == Some(fid)) {
                        rows = self.thread_rows(rows, ix, px(24.), cx);
                    }
                }
            }
            for ix in (0..self.slots.len())
                .filter(|ix| places[*ix].project == Some(pid) && places[*ix].folder.is_none())
            {
                rows = self.thread_rows(rows, ix, px(12.), cx);
            }
        }
        // Every thread belongs to a project; past sessions from elsewhere
        // show once their folder is added as one.
        rows.into_any_element()
    }

    /// A thread's row, then its subagents unless they're folded away.
    fn thread_rows(&self, mut rows: Div, ix: usize, indent: Pixels, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let slot = &self.slots[ix];
        let active = ix == self.active;
        let selected = active && self.active_child.is_none();
        let view = cx.entity();
        let menu_view = view.clone();
        let parent = slot.view().map(|view| view.read(cx).id);
        let family = parent.and_then(|parent| self.families.get(&parent));
        let visible: Vec<(usize, &Child)> = family
            .map(|family| {
                family
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, child)| !child.hidden)
                    .collect()
            })
            .unwrap_or_default();
        let collapsed = family.is_some_and(|family| family.collapsed);
        rows = rows.child(
            h_flex()
                .id(("chat", ix))
                .ml(indent)
                .on_drag(
                    DraggedThread {
                        thread: match slot {
                            Slot::Live(view) => ThreadKey::Chat(view.read(cx).id),
                            Slot::Past(past) => ThreadKey::Past(past.session.clone()),
                        },
                        title: slot.title(cx),
                    },
                    |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                )
                .gap_1()
                .px_2()
                .py_1p5()
                .rounded(theme.radius)
                .text_sm()
                .when(selected, |this| this.bg(theme.sidebar_accent))
                .hover(|this| this.bg(theme.sidebar_accent))
                .on_click(cx.listener(move |this, _, window, cx| this.click_chat(ix, window, cx)))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.preload(ix);
                    }
                    this.hover_thread(*hovered, (ix, None), cx);
                }))
                .child(div().flex_1().min_w_0().truncate().child(slot.title(cx)))
                .when(!visible.is_empty() && collapsed, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("+{}", visible.len())),
                    )
                })
                .when(selected, |this| {
                    this.child(
                        Button::new(("delete-chat", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Delete)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                view.update(cx, |this, cx| this.delete_chat(ix, window, cx));
                            }),
                    )
                })
                .context_menu(move |menu, _, _| {
                    let view = menu_view.clone();
                    menu.item(
                        PopupMenuItem::new("Delete chat")
                            .icon(IconName::Delete)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.delete_chat(ix, window, cx))
                            }),
                    )
                }),
        );
        if collapsed {
            return rows;
        }
        let last = visible.len().saturating_sub(1);
        for (n, (cix, child)) in visible.iter().enumerate() {
            let cix = *cix;
            let viewing = active && self.active_child.as_ref() == Some(&child.view);
            let line = theme.border;
            let continues = n < last;
            let view = cx.entity();
            rows = rows.child(
                h_flex()
                    .id(("subagent", ix * 1000 + cix))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.hover_thread(*hovered, (ix, Some(cix)), cx)
                    }))
                    .ml(indent)
                    .group("subagent-row")
                    .h(px(30.))
                    .child(
                        // The fork: a line down from the parent that
                        // curves into each child, continuing past all
                        // but the last.
                        canvas(
                            |_, _, _| (),
                            move |bounds, _, window, _| {
                                // A 1px stroke on half-pixel centers covers
                                // whole device pixels: no anti-aliased blur.
                                let x = (bounds.left() + px(14.)).floor() + px(0.5);
                                let mid = (bounds.top() + bounds.size.height / 2.).floor() + px(0.5);
                                let radius = px(8.);
                                let mut path = PathBuilder::stroke(px(1.));
                                path.move_to(point(x, bounds.top() - px(2.)));
                                path.line_to(point(x, mid - radius));
                                path.curve_to(point(x + radius, mid), point(x, mid));
                                path.line_to(point(bounds.right() - px(2.), mid));
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, line);
                                }
                                if continues {
                                    let mut rest = PathBuilder::stroke(px(1.));
                                    rest.move_to(point(x, mid - radius));
                                    rest.line_to(point(x, bounds.bottom() + px(2.)));
                                    if let Ok(rest) = rest.build() {
                                        window.paint_path(rest, line);
                                    }
                                }
                            },
                        )
                        .w(px(28.))
                        .h_full()
                        .flex_shrink_0(),
                    )
                    .child(
                        h_flex()
                            .id(("subagent-pill", ix * 1000 + cix))
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .text_xs()
                            .when(viewing, |this| this.bg(theme.sidebar_accent))
                            .hover(|this| this.bg(theme.sidebar_accent))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.active != ix {
                                    this.select(ix, window, cx);
                                }
                                this.select_child(cix, window, cx);
                            }))
                            .when(child.running, |this| this.child(Spinner::new().xsmall()))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(child.view.read(cx).title.clone()),
                            )
                            .child(
                                div()
                                    .invisible()
                                    .group_hover("subagent-row", |style| style.visible())
                                    .child(
                                        Button::new(("hide-subagent", ix * 1000 + cix))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::EyeOff)
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                view.update(cx, |this, cx| {
                                                    if this.active != ix {
                                                        this.select(ix, window, cx);
                                                    }
                                                    this.hide_child(cix, window, cx)
                                                });
                                            }),
                                    ),
                            ),
                    ),
            );
        }
        rows
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let chats = self.render_chat_tree(cx);

        let account_label: SharedString = match self.account {
            Account::Unknown => "".into(),
            Account::SignedOut => "Not signed in".into(),
            Account::SigningIn => "Signing in…".into(),
            Account::SignedIn { from_env: true } => "OpenRouter · env key".into(),
            Account::SignedIn { from_env: false } => "OpenRouter".into(),
        };
        let signed_in = matches!(self.account, Account::SignedIn { .. });
        let appearance = self.library.appearance;
        let refreshing = self.refreshing_models;
        let models = self.models.clone();
        let default = self.default_model();
        let model_count = self.models.len();
        let view = cx.entity();
        let account_menu = Button::new("account")
            .ghost()
            .w_full()
            .label(account_label)
            .icon(IconName::CircleUser)
            // Opens upward: the button sits at the bottom of the window.
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let view = view.clone();
                // The model new threads start on (the composer can pick
                // another for one thread): searched like the composer's.
                let default_label = format!(
                    "Default model · {}",
                    crate::models::label(&models, &default)
                );
                let menu = {
                    let view = view.clone();
                    menu.item(
                        PopupMenuItem::new(default_label).on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| this.show_default_picker(true, window, cx))
                        }),
                    )
                };
                let menu = menu.separator();
                let menu = [
                    ("Match system", Appearance::System),
                    ("Light", Appearance::Light),
                    ("Dark", Appearance::Dark),
                ]
                .into_iter()
                .fold(menu.label("Appearance"), |menu, (label, choice)| {
                    let view = view.clone();
                    menu.item(
                        PopupMenuItem::new(label)
                            .checked(appearance == choice)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.set_appearance(choice, window, cx));
                            }),
                    )
                })
                .separator();
                // Fetched only when asked, in the background.
                let menu = {
                    let view = view.clone();
                    let label = if refreshing {
                        "Refreshing models…".to_string()
                    } else if model_count == 0 {
                        "Refresh models".to_string()
                    } else {
                        format!("Refresh models ({model_count})")
                    };
                    menu.item(PopupMenuItem::new(label).disabled(refreshing).on_click(
                        move |_, _, cx| view.update(cx, |this, cx| this.refresh_models(cx)),
                    ))
                    .separator()
                };
                if signed_in {
                    menu.item(
                        PopupMenuItem::new("Sign out").on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| this.sign_out(window, cx));
                        }),
                    )
                } else {
                    menu.item(PopupMenuItem::new("Sign in with OpenRouter").on_click(
                        move |_, window, cx| {
                            view.update(cx, |this, cx| this.sign_in(window, cx));
                        },
                    ))
                }
            });

        let status = self.hovered_status(cx);
        // A plain column rather than gpui-kit's Sidebar, whose slots take
        // only its own menu items: the chat list is a tree with connectors.
        let theme = cx.theme();
        v_flex()
            .w(px(self.sidebar_width))
            .h_full()
            .flex_shrink_0()
            // Frosted: the window's blur shows through a tint.
            .bg(theme.background.alpha(SIDEBAR_TINT))
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .relative()
            .children(status)
            .child(
                div()
                    .id("chat-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    // Clears the traffic lights in the transparent title bar.
                    .pt(px(TRAFFIC_LIGHT_CLEARANCE))
                    .child(chats),
            )
            .child(div().relative().w_full().p_2().child(account_menu).when(
                self.default_picker_open,
                |this| {
                    let theme = cx.theme();
                    this.child(deferred(
                        div()
                            .id("default-model-picker")
                            .absolute()
                            .bottom(px(44.))
                            .left(px(8.))
                            .p_2()
                            .rounded(theme.radius_lg)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .shadow_lg()
                            .occlude()
                            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                                this.show_default_picker(false, window, cx)
                            }))
                            .child(self.default_picker.clone()),
                    ))
                },
            ))
            .into_any_element()
    }
}

impl Workspace {
    /// Follows a divider drag, keeping each pane within its limits and the
    /// chat at least `CHAT_MIN` wide.
    fn drag_divider(&mut self, divider: Divider, x: f32, window: &Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let panel = if self.panel_open {
            self.panel_width
        } else {
            0.
        };
        match divider {
            Divider::Sidebar => {
                let max = SIDEBAR_MAX.min(width - panel - CHAT_MIN);
                self.sidebar_width = x.min(max).max(SIDEBAR_MIN);
            }
            Divider::Panel => {
                let max = width - self.sidebar_width - CHAT_MIN;
                self.panel_width = (width - x).min(max).max(PANEL_MIN);
            }
        }
        cx.notify();
    }

    /// The grab area of a divider: a strip centered on the edge at `x`.
    fn divider(&self, divider: Divider, id: &'static str) -> Stateful<Div> {
        div()
            .id(id)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(DIVIDER_GRAB))
            .cursor_col_resize()
            .on_drag(divider, |divider, _, _, cx| cx.new(|_| *divider))
    }
}

/// How long the text size shows after it changes, and its fades.
const BADGE_SHOWN: std::time::Duration = std::time::Duration::from_millis(1100);
const BADGE_FADE_IN: f32 = 0.12;
const BADGE_FADE_OUT: f32 = 0.4;

impl Workspace {
    /// The size the text just changed to, centered over the window: fades
    /// in, holds, fades out.
    fn text_size_badge(&mut self, window: &mut Window) -> Option<AnyElement> {
        let (size, since) = self.text_size_badge?;
        let age = since.elapsed();
        if age >= BADGE_SHOWN {
            self.text_size_badge = None;
            return None;
        }
        window.request_animation_frame();
        let t = age.as_secs_f32();
        let opacity = (t / BADGE_FADE_IN)
            .min((BADGE_SHOWN.as_secs_f32() - t) / BADGE_FADE_OUT)
            .min(1.);
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("text-size")
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .bg(gpui_kit::black().alpha(0.7))
                        .text_color(gpui_kit::white())
                        .text_2xl()
                        .opacity(opacity)
                        .child(format!("{size:.0}")),
                )
                .into_any_element(),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        let sidebar = self.render_sidebar(cx);
        // The theme's background is clear for the sidebar's sake; the
        // content panes stay solid.
        let solid = cx.theme().background.alpha(1.);
        h_flex()
            .size_full()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<Divider>, window, cx| {
                    let divider = *event.drag(cx);
                    this.drag_divider(divider, f32::from(event.event.position.x), window, cx);
                }),
            )
            .child(sidebar)
            .child(v_flex().flex_1().min_w_0().h_full().bg(solid).map(|this| {
                match self.active_view() {
                    Some(chat) => this.child(chat),
                    // Threads live in projects: with none, the first
                    // step is adding one.
                    None => this.items_center().justify_center().child(
                        Button::new("add-first-project")
                            .label("Add a project")
                            .icon(IconName::FolderOpen)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_project(window, cx)),
                            ),
                    ),
                }
            }))
            .when(self.panel_open, |this| {
                this.child(
                    div()
                        .w(px(self.panel_width))
                        .h_full()
                        .flex_shrink_0()
                        .bg(solid)
                        .child(self.panel.clone()),
                )
            })
            .child(
                self.divider(Divider::Sidebar, "sidebar-divider")
                    .left(px(self.sidebar_width - DIVIDER_GRAB / 2.)),
            )
            .when(self.panel_open, |this| {
                this.child(
                    self.divider(Divider::Panel, "panel-divider")
                        .left(px(width - self.panel_width - DIVIDER_GRAB / 2.)),
                )
            })
            .children(self.text_size_badge(window))
            .child(
                div().absolute().top(px(8.)).right(px(12.)).child(
                    Button::new("toggle-panel")
                        .ghost()
                        .small()
                        .icon(IconName::PanelRight)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panel_open = !this.panel_open;
                            cx.notify();
                        })),
                ),
            )
    }
}

/// Screenshots and synthetic input. Input goes straight to this window, never
/// through the OS, so the user's cursor and keyboard are untouched.
#[cfg(feature = "automation")]
fn window_command(
    command: &serde_json::Value,
    window: &mut Window,
    cx: &mut App,
) -> serde_json::Value {
    use serde_json::json;
    match command["cmd"].as_str().unwrap_or_default() {
        // Renders the current state, not whatever frame was last shown: a
        // background window may not have drawn since the state changed.
        "screenshot" => {
            let path = command["path"].as_str().unwrap_or("/tmp/sense.png");
            window.draw(cx).clear(cx);
            match window
                .render_to_image()
                .and_then(|image| Ok(image.save(path)?))
            {
                Ok(()) => json!({ "ok": path }),
                Err(err) => json!({ "error": err.to_string() }),
            }
        }
        // Draws one frame, as the window would next, and counts the pixels
        // of a color (within `tolerance` per channel): whether something
        // drawn from an image is on screen in that frame.
        "count_color" => {
            let want: Vec<u8> = command["rgb"]
                .as_array()
                .map(|rgb| rgb.iter().map(|c| c.as_u64().unwrap_or(0) as u8).collect())
                .unwrap_or_default();
            let tolerance = command["tolerance"].as_u64().unwrap_or(8) as i32;
            window.draw(cx).clear(cx);
            match window.render_to_image() {
                Ok(image) => {
                    let count = image
                        .pixels()
                        .filter(|pixel| {
                            want.len() == 3
                                && (0..3)
                                    .all(|c| (pixel[c] as i32 - want[c] as i32).abs() <= tolerance)
                        })
                        .count();
                    json!({ "count": count })
                }
                Err(err) => json!({ "error": err.to_string() }),
            }
        }
        // Window coordinates in logical pixels; screenshots are 2x on Retina.
        "click" => {
            let position = point(
                px(command["x"].as_f64().unwrap_or(0.) as f32),
                px(command["y"].as_f64().unwrap_or(0.) as f32),
            );
            let modifiers = Modifiers::default();
            let events = [
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers,
                }),
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                }),
            ];
            for event in events {
                window.dispatch_event(event, cx);
            }
            json!({ "ok": true })
        }
        // Nothing: with `timed`, the cost of drawing the current frame.
        "frame" => json!({ "ok": true }),
        // The formulas drawn as images so far, after drawing the current frame.
        "math" => {
            window.draw(cx).clear(cx);
            crate::math::automation_report()
        }
        // A mouse drag from one point to another in steps, as a hand would
        // do it: press, move with the button held, release. Each step is
        // drawn before the next, so hit tests see the moved layout.
        "drag" => {
            let at = |key: &str| {
                point(
                    px(command[key][0].as_f64().unwrap_or(0.) as f32),
                    px(command[key][1].as_f64().unwrap_or(0.) as f32),
                )
            };
            let (from, to) = (at("from"), at("to"));
            let modifiers = Modifiers::default();
            let dispatch = |event, window: &mut Window, cx: &mut App| {
                window.dispatch_event(event, cx);
                window.draw(cx).clear(cx);
            };
            dispatch(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from,
                    pressed_button: None,
                    modifiers,
                }),
                window,
                cx,
            );
            dispatch(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                window,
                cx,
            );
            const STEPS: usize = 8;
            for step in 1..=STEPS {
                let t = step as f32 / STEPS as f32;
                dispatch(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: from + (to - from) * t,
                        pressed_button: Some(MouseButton::Left),
                        modifiers,
                    }),
                    window,
                    cx,
                );
            }
            dispatch(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers,
                    click_count: 1,
                }),
                window,
                cx,
            );
            json!({ "ok": true })
        }
        // Hovers (or clicks) a point and reports the rectangles that newly
        // appear, in logical window pixels. Layout checks build on this.
        "probe" => {
            let position = point(
                px(command["x"].as_f64().unwrap_or(0.) as f32),
                px(command["y"].as_f64().unwrap_or(0.) as f32),
            );
            window.draw(cx).clear(cx);
            let before = visible_quads(window);
            let modifiers = Modifiers::default();
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers,
                }),
                cx,
            );
            if command["click"].as_bool().unwrap_or(false) {
                for event in [
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 1,
                        first_mouse: false,
                    }),
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers,
                        click_count: 1,
                    }),
                ] {
                    window.dispatch_event(event, cx);
                }
            }
            window.draw(cx).clear(cx);
            let appeared: Vec<_> = visible_quads(window)
                .into_iter()
                .filter(|quad| !before.contains(quad))
                .map(|(rect, _)| rect)
                .collect();
            json!({ "appeared": appeared })
        }
        // Rectangles cut short sideways by a clip: content that doesn't fit
        // its container (vertical clipping is just scrolling).
        "clipped" => {
            window.draw(cx).clear(cx);
            let scale = window.scale_factor();
            let clipped: Vec<_> = window
                .painted_quads()
                .into_iter()
                .filter_map(|quad| {
                    let visible = quad.bounds.intersect(&quad.content_mask.bounds);
                    let cut = quad.bounds.size.width.0 - visible.size.width.0;
                    (visible.size.height.0 > 0. && visible.size.width.0 > 0. && cut > scale).then(
                        || {
                            let logical = |v: f32| (v / scale).round() as i32;
                            [
                                logical(quad.bounds.origin.x.0),
                                logical(quad.bounds.origin.y.0),
                                logical(quad.bounds.size.width.0),
                                logical(visible.size.width.0),
                            ]
                        },
                    )
                })
                .collect();
            json!({ "clipped": clipped })
        }
        // Every rectangle drawn now, in logical window pixels.
        "quads" => {
            window.draw(cx).clear(cx);
            let quads: Vec<_> = visible_quads(window)
                .into_iter()
                .map(|(rect, _)| rect)
                .collect();
            json!({ "quads": quads })
        }
        // Space-separated keystrokes, e.g. "cmd-n" or "h i enter".
        "key" => {
            for key in command["keys"]
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
            {
                match Keystroke::parse(key) {
                    Ok(keystroke) => _ = window.dispatch_keystroke(keystroke, cx),
                    Err(err) => return json!({ "error": err.to_string() }),
                }
            }
            json!({ "ok": true })
        }
        other => json!({ "error": format!("unknown command {other:?}") }),
    }
}

/// Painted rectangles clipped to what is actually visible, as
/// `([x, y, w, h], style)` in logical pixels, rounded to whole pixels.
#[cfg(feature = "automation")]
fn visible_quads(window: &Window) -> Vec<([i32; 4], String)> {
    let scale = window.scale_factor();
    window
        .painted_quads()
        .into_iter()
        .filter_map(|quad| {
            let visible = quad.bounds.intersect(&quad.content_mask.bounds);
            let (origin, size) = (visible.origin, visible.size);
            if size.width.0 <= 0. || size.height.0 <= 0. {
                return None;
            }
            let logical = |value: f32| (value / scale).round() as i32;
            let rect = [
                logical(origin.x.0),
                logical(origin.y.0),
                logical(size.width.0),
                logical(size.height.0),
            ];
            Some((
                rect,
                format!("{:?}{:?}", quad.background, quad.border_color),
            ))
        })
        .collect()
}

/// fx keeps each session under `~/.fx/sessions/<id>`. The id comes from the
/// agent, so anything that could leave that folder is rejected.
fn fx_session_dir(id: &str) -> Option<PathBuf> {
    let safe = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    let home = std::env::var_os("HOME")?;
    safe.then(|| PathBuf::from(home).join(".fx/sessions").join(id))
}

/// A duration as read at a glance: "12s", "4m", "2h".
fn elapsed(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

fn notice_text(notice: &Notice) -> String {
    match notice {
        Notice::AgentStopped => "The agent stopped.".into(),
        Notice::SignedOut => "Signed out.".into(),
        Notice::NeedsSignIn => "Sign in to OpenRouter to send this.".into(),
        Notice::SessionFailed(error) => format!("Couldn't start a session: {error}"),
        Notice::TurnFailed(error) => format!("The turn failed: {error}"),
        Notice::BaselineFailed(error) => format!("Changes won't be shown: {error}"),
    }
}

/// Keychain first; `OPENROUTER_API_KEY` is a development fallback.
fn load_account() -> Option<(String, bool)> {
    if let Some(account) = sense_auth::load() {
        return Some((account.api_key, false));
    }
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| (key, true))
}
