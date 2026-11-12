//! the window.
//!
//! a mac three-pane window: a sidebar of sessions with the ones that need you
//! on top, the open session's transcript, and an inspector that stays out of
//! the way until asked for. this file draws `Model` and turns what the user
//! does into `Cmd`s. it decides nothing about sessions.
//!
//! motion follows two rules. only changes move, and they move from where the
//! thing was. and nothing in a list moves while the pointer rests on it.

mod compose;
mod composer;
mod exec;
mod mentions;
mod backdrop;
mod space;
mod models;
mod looks;
mod files;
mod git;
mod palette;
mod panel;
mod panes;
mod projects;
mod settings;
mod sidebar;
mod surface;
mod terminal;

pub(crate) use compose::{Drawer, TranscriptOpts};
pub(crate) use panel::{Tab, Workspace};

use crate::bridge::{Bridge, Cmd, UiEvent};
use crate::model::{Change, Connection, Entry, Model, Open, Rect, SidebarLayout, ago, grid, grid_cols, grid_step, project_name};
use crate::ui::Show;
use crate::motion::{PANEL, SMOOTH, Spring, Springs, Ticker, fly};
use crate::theme::{self, Palette, Type, Typeset as _};
use gpui_kit::base::{InteractiveElementExt as _, MotionReveal};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tessel_proto::activity::summarize_tool;
use tessel_proto::{
    Attachment, AttentionReason, ItemBody, ItemId, Millis, ModelInfo, NoticeLevel, PermissionDecision, PermissionRequest, RequestId,
    RunState, SessionId, SessionSummary, ToolStatus,
};
#[cfg(feature = "rich")]
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
#[cfg(feature = "rich")]
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// how the app was asked to start.
#[derive(Default)]
pub struct Launch {
    /// open the session whose id starts with this.
    pub open: Option<String>,
    /// start with the sidebar and inspector hidden.
    pub focus: bool,
    /// force light (`Some(false)`) or dark (`Some(true)`) instead of following the system.
    pub dark: Option<bool>,
    /// start on the overview of all sessions.
    pub overview: bool,
    /// start with the workspace panel open on this tab.
    pub panel: Option<String>,
    /// open this file in the panel's editor once a session is showing.
    pub file: Option<String>,
    /// run every transition once, print how the frames went, and quit.
    pub bench_motion: bool,
    /// pour output through a terminal pane, print how the frames went, and quit.
    pub bench_term: bool,
    /// measure startup: print how long until the first frame that shows
    /// sessions from the daemon, then quit. counted from this instant.
    pub bench_from: Option<Instant>,
    /// the layout file to draw from and watch. without one the window uses
    /// the default layout and never looks at the disk (tests rely on that).
    pub ui: Option<PathBuf>,
}

/// a divider being dragged.
pub(crate) struct Drag {
    /// the drawer whose edge it is.
    drawer: String,
    start_x: f32,
    start_w: f32,
    /// the last two pointer samples, for the speed at release.
    last: (f32, Instant),
    velocity: f32,
}

pub struct Shell {
    pub(crate) model: Model,
    bridge: Bridge,
    /// the colours in use. a cell because drawing swaps it for a while: a
    /// part of the layout on a background the theme didn't plan for is drawn
    /// with inks made for that background.
    pub(crate) palette: std::cell::Cell<Palette>,
    /// the fill of the node being drawn, resolved before its inks were swapped.
    bg_now: std::cell::Cell<Option<Hsla>>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<gpui_kit::component::input::TextareaState>,
    /// the project a new session starts in, when one was picked; otherwise
    /// the latest (see `new_folder`).
    pub(crate) new_project: Option<String>,
    new_prompt: Entity<InputState>,
    pub(crate) new_worktree: bool,
    /// whether a folder is in a git repository, as the daemon said; `None`
    /// while it's being asked. a folder that isn't one can't have a worktree.
    pub(crate) folder_git: std::cell::RefCell<HashMap<String, Option<bool>>>,
    /// the folder last asked about: the answer doesn't name it.
    folder_asked: std::cell::RefCell<Option<String>>,
    show_new: bool,

    // the layout, from ui.json, and the state it implies.
    pub(crate) ui: Rc<crate::ui::Ui>,
    ui_path: Option<PathBuf>,
    ui_stamp: Option<std::time::SystemTime>,
    /// what is wrong with the layout file, while something is.
    ui_error: Option<String>,
    forced_dark: Option<bool>,
    /// the side panels, each with a width it wants when open and a spring for the width it has now.
    pub(crate) drawers: Vec<Drawer>,
    /// the drawer the workspace panel is in, if it is in one.
    panel_drawer: Option<String>,
    has_panel: bool,
    pub(crate) panel_tabs: Vec<Tab>,
    transcript_opts: TranscriptOpts,
    sessions_trace: bool,
    // the settings page.
    settings_open: bool,
    /// which page of settings is showing.
    pub(crate) settings_section: &'static str,
    focus_settings: bool,
    settings_focus: FocusHandle,
    confirm_reset: bool,
    font_input: Entity<InputState>,
    mono_input: Entity<InputState>,
    /// what is typed in the model picker's search.
    pub(crate) model_search: Entity<InputState>,
    drag: Option<Drag>,
    // panes: which are folded away, where each is drawn, the room they have,
    // the gaps between them and the one being dragged.
    pub(crate) pane_folded: HashMap<String, bool>,
    pane_fx: HashMap<String, panes::PaneFx>,
    pane_area: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    pane_gaps: Vec<(Vec<usize>, usize, crate::ui::Axis, Rect, (f32, f32), f32)>,
    pane_drag: Option<panes::PaneDrag>,
    /// the pane last clicked in: the one keys act on.
    pub(crate) pane_focus: Option<String>,
    /// the pane maximized over the others, if one is.
    pub(crate) pane_zoom: Option<String>,
    /// a pane being carried by its grip.
    pane_move: Option<panes::PaneMove>,
    /// the window's own focus, held when nothing inside has it, so keys always arrive.
    root_focus: FocusHandle,
    /// window commands scripts sent, run on the next frame (they need the window).
    ui_calls: Vec<(u64, String, serde_json::Value)>,
    /// someone listens to what this window publishes.
    ui_watched: bool,
    /// a Surfaces pane is on screen, so pane surfaces live there, not in the chat.
    surfaces_docked: bool,
    /// the daemon's own settings; none until it says (or if it's too old to).
    pub(crate) daemon_settings: Option<tessel_proto::DaemonSettings>,
    /// terminals the daemon runs, as this window draws them.
    terms: HashMap<tessel_proto::term::TermId, terminal::TermCell>,
    /// which terminal a pane shows, per open session.
    pane_terms: HashMap<(String, Option<SessionId>), tessel_proto::term::TermId>,
    /// panes waiting on a terminal the daemon is starting for them.
    term_opening: std::collections::HashSet<(String, Option<SessionId>)>,
    /// a terminal pane that takes the keys as soon as its shell is up.
    term_focus: Option<String>,
    git: git::GitView,
    bench_term: Option<terminal::BenchTerm>,
    /// a theme being tried on: drawn, not written.
    theme_preview: Option<String>,
    /// the preview is the palette's: closing it takes it off.
    theme_preview_by_palette: bool,
    /// the commit went in: the message box empties (it needs a window to).
    git_clear_message: bool,
    /// a message written for what's staged, for the message box.
    git_suggestion: Option<String>,
    pub(crate) palette_ui: palette::PaletteState,
    /// the pointer is over the transcript's message rail.
    rail_open: bool,
    // making or cloning a project.
    sheet: Option<projects::Sheet>,
    focus_sheet: bool,
    sheet_focus: FocusHandle,
    sheet_error: Option<String>,
    making: bool,
    project_name: Entity<InputState>,
    project_parent: Entity<InputState>,
    github_repo: Entity<InputState>,
    template: projects::Template,
    github_private: bool,
    /// what a session is being renamed to, and which one.
    rename_input: Entity<InputState>,
    pub(crate) renaming: Option<SessionId>,

    // the workspace panel, for the open session. what it showed for other
    // sessions is parked, so coming back finds the same file and any edits.
    pub(crate) ws: Workspace,
    pub(crate) ws_session: Option<SessionId>,
    ws_parked: HashMap<SessionId, Workspace>,

    /// files that will go with the next message.
    pub(crate) attachments: Vec<Attachment>,
    /// which changes the changes list shows: "all" uncommitted, this "session"'s, or the last "turn"'s.
    pub(crate) changes_scope: &'static str,
    /// narrows the changes list by path.
    pub(crate) changes_search: Entity<InputState>,
    /// the dithered backdrop behind the panes, once drawn.
    pub(crate) backdrop: backdrop::Shared,
    /// the felt under the window, once drawn.
    pub(crate) felt: backdrop::Shared,
    /// the space backdrop's photo.
    pub(crate) space: space::Shared,
    /// pasted and attached pictures, decoded once: by where their bytes live.
    pub(crate) thumbs: std::cell::RefCell<HashMap<(usize, usize), Arc<Image>>>,
    /// how many images have been pasted, to name the next one.
    pasted: u32,
    /// the models the agent offers. asked for once connected.
    pub(crate) models: Vec<ModelInfo>,
    /// the agent's slash commands, for the composer's `/` menu.
    pub(crate) slash_commands: Vec<tessel_proto::SlashCommand>,
    /// the row chosen in the composer's `/` or `@` menu.
    pub(crate) mention_sel: usize,
    /// escape closed the menu for exactly this text; typing opens it again.
    pub(crate) mention_dismissed: Option<String>,
    /// finished thoughts the user has opened to read in full.
    thoughts_open: HashSet<ItemId>,
    /// diagrams and formulas already drawn, by appearance and source. drawing
    /// happens while a row is being laid out, hence the cell.
    #[cfg(feature = "rich")]
    drawn: RefCell<HashMap<(bool, bool, String), Drawn>>,

    // the sidebar as currently drawn, and rows still travelling to their places.
    shown: SidebarLayout,
    row_fx: Springs<SessionId>,
    /// the pointer is resting on the list, so rows hold their places.
    sidebar_hovered: bool,

    // the overview: every session as a card. each card is a frame that flies
    // between its sidebar row and its place in the grid.
    overview: bool,
    /// how present the overview is, 0 to 1.
    overview_fade: Spring,
    /// each card's frame: x, y, width, height.
    cards: HashMap<SessionId, [Spring; 4]>,
    card_order: Vec<SessionId>,
    /// the card the arrow keys are on.
    overview_cursor: usize,
    overview_focus: FocusHandle,
    /// the overview just opened and should take the keyboard.
    focus_overview: bool,
    /// the window's size as of the last frame, in points.
    viewport: (f32, f32),
    /// how long the display gives each frame, in milliseconds.
    refresh_ms: f32,

    /// how far open the prompt bar is, 0 to 1.
    prompt_open: Spring,
    /// what the prompt bar showed last, so it can close gracefully after the
    /// request it was showing has gone.
    prompt_ghost: Vec<PermissionRequest>,

    ticker: Ticker,
    window_active: bool,
    error: Option<String>,
    /// the session whose delete button has been pressed once. deleting takes two presses.
    pub(crate) confirm_delete: Option<SessionId>,
    /// answers picked so far for a question that has several parts.
    answers: HashMap<RequestId, serde_json::Map<String, serde_json::Value>>,
    open_on_launch: Option<String>,
    panel_on_launch: Option<String>,
    file_on_launch: Option<String>,
    overview_on_launch: bool,
    /// set while the motion benchmark runs: which transition is being
    /// measured, and when each frame started and whether it was mid-animation.
    bench: Option<(&'static str, Vec<(&'static str, Instant, bool)>)>,
    /// frames drawn during the benchmark while the window was not in front.
    /// the system slows a background window to 30 frames a second, so any of
    /// these make the measurement meaningless.
    bench_inactive_frames: usize,
    bench_on_launch: bool,
    bench_from: Option<Instant>,
    have_sessions: bool,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

/// a drawn picture and its natural size, or why it could not be drawn.
#[cfg(feature = "rich")]
type Drawn = Result<(Arc<Image>, f32, f32), String>;

fn now_ms() -> Millis {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as Millis).unwrap_or(0)
}

impl Shell {
    pub fn new(
        bridge: Bridge,
        mut events: mpsc::UnboundedReceiver<UiEvent>,
        launch: Launch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let forced = launch.dark;
        let (ui, ui_stamp, ui_error) = match &launch.ui {
            Some(path) => {
                let (loaded, stamp) = compose::load_ui(path);
                match loaded {
                    Ok(ui) => (ui, stamp, None),
                    Err(e) => (crate::ui::Ui::default_layout(), stamp, Some(e)),
                }
            }
            None => (crate::ui::Ui::default_layout(), None, None),
        };
        let palette = theme::apply(forced, &ui.theme, window, cx);
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        // three lines to start, growing with what's written; return sends, shift-return is a new line.
        let composer = cx.new(|cx| gpui_kit::component::input::TextareaState::new(window, cx).auto_grow(3, 12).submit_on_enter(true).placeholder("Ask, or tell the agent what to do…"));
        let new_prompt = cx.new(|cx| InputState::new(window, cx).placeholder("What should it do?"));
        let font_input = cx.new(|cx| InputState::new(window, cx).placeholder("System"));
        let mono_input = cx.new(|cx| InputState::new(window, cx).placeholder(theme::MONO));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models"));
        let project_name = cx.new(|cx| InputState::new(window, cx).placeholder("my-project"));
        let rename_input = cx.new(|cx| InputState::new(window, cx));
        let palette_ui = palette::PaletteState::new(window, cx);
        let palette_input = palette_ui.input.clone();
        let changes_search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let projects_home = std::env::var("TESSEL_PROJECTS").unwrap_or_else(|_| "~/tessel/projects".into());
        let project_parent = cx.new(|cx| InputState::new(window, cx).default_value(projects_home));
        let github_repo = cx.new(|cx| InputState::new(window, cx).placeholder("owner/repo"));

        // the `/` and `@` menus get their keys before the message box does:
        // here bindings (return sends, arrows move) run ahead of key listeners.
        let menu_keys = {
            let (me, composer) = (cx.entity().downgrade(), composer.clone());
            cx.intercept_keystrokes(move |ev, window, cx| {
                if ev.keystroke.modifiers.modified() || !composer.focus_handle(cx).is_focused(window) {
                    return;
                }
                let key = ev.keystroke.key.clone();
                if me.update(cx, |this, cx| this.mention_key(&key, window, cx)).unwrap_or(false) {
                    cx.stop_propagation();
                }
            })
        };
        let subscriptions = vec![
            menu_keys,
            // the changes list narrows as you type.
            cx.subscribe_in(&changes_search, window, |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send(window, cx);
                }
            }),
            cx.subscribe_in(&new_prompt, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.create(window, cx);
                }
            }),
            // the palette: typing searches, return runs.
            cx.subscribe_in(&palette_input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    this.palette_ui_reset();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.palette_confirm(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&rename_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev
                    && let Some(id) = this.renaming.take()
                {
                    this.rename_session(id, cx);
                    // the menu closes once the name is in.
                    window.focus(&this.root_focus, cx);
                }
            }),
            // what a project will be called shows as it is typed.
            cx.subscribe_in(&project_name, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&github_repo, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
            // the model list filters as you type.
            cx.subscribe_in(&model_search, window, |_, _, ev: &InputEvent, _, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&font_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.apply_fonts(window, cx);
                }
            }),
            cx.subscribe_in(&mono_input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.apply_fonts(window, cx);
                }
            }),
            // light and dark follow the system, live.
            cx.observe_window_appearance(window, move |this, window, cx| {
                this.palette.set(theme::apply(forced, &this.ui.theme, window, cx));
                cx.notify();
            }),
        ];

        // the bridge thread hands us events; apply everything that has
        // arrived, then repaint once. a burst of tokens is one frame, not hundreds.
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = events.recv().await {
                let mut batch = vec![first];
                while batch.len() < 4096
                    && let Ok(more) = events.try_recv()
                {
                    batch.push(more);
                }
                let alive = this.update_in(cx, |this, window, cx| {
                    for ev in batch {
                        this.apply(ev, cx);
                    }
                    // commands from scripts run as soon as they arrive, with the window to hand.
                    if !this.ui_calls.is_empty() {
                        this.run_ui_calls(window, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // the working glyph steps while anything works, and the window is left
        // alone when nothing does.
        let beat = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(BEAT_MS)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.model.sessions.iter().any(|s| matches!(s.run_state, RunState::Working | RunState::Starting)) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // "waiting 4m" has to keep counting with nothing else happening, and
        // a saved layout file should show up without anything else happening.
        let clock = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    this.workspace_tick();
                    this.poll_ui(window, cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let mut this = Self {
            model: Model::default(),
            bridge,
            palette: std::cell::Cell::new(palette),
            bg_now: std::cell::Cell::new(None),
            scroller,
            composer,
            new_project: std::env::var("TESSEL_DEFAULT_CWD").ok().filter(|d| !d.is_empty()),
            new_prompt,
            // each session in a workspace of its own, where the folder allows.
            new_worktree: true,
            folder_git: Default::default(),
            folder_asked: Default::default(),
            show_new: true,
            ui: Rc::new(crate::ui::Ui::default_layout()),
            ui_path: launch.ui.clone(),
            ui_stamp,
            ui_error,
            forced_dark: forced,
            drawers: Vec::new(),
            panel_drawer: None,
            has_panel: false,
            panel_tabs: Tab::ALL.to_vec(),
            transcript_opts: TranscriptOpts::default(),
            sessions_trace: true,
            settings_open: false,
            settings_section: "appearance",
            focus_settings: false,
            settings_focus: cx.focus_handle(),
            confirm_reset: false,
            font_input,
            mono_input,
            model_search,
            drag: None,
            pane_folded: HashMap::new(),
            pane_fx: HashMap::new(),
            pane_area: Rc::new(std::cell::Cell::new(None)),
            pane_gaps: Vec::new(),
            pane_drag: None,
            pane_focus: None,
            pane_zoom: None,
            pane_move: None,
            root_focus: cx.focus_handle(),
            ui_calls: Vec::new(),
            ui_watched: false,
            surfaces_docked: false,
            daemon_settings: None,
            terms: HashMap::new(),
            pane_terms: HashMap::new(),
            term_opening: Default::default(),
            term_focus: None,
            git: git::GitView::new(window, cx),
            bench_term: launch.bench_term.then(terminal::BenchTerm::default),
            theme_preview: None,
            theme_preview_by_palette: false,
            git_clear_message: false,
            git_suggestion: None,
            palette_ui,
            rail_open: false,
            sheet: None,
            focus_sheet: false,
            sheet_focus: cx.focus_handle(),
            sheet_error: None,
            making: false,
            project_name,
            project_parent,
            github_repo,
            template: projects::Template::default(),
            github_private: false,
            rename_input: rename_input.clone(),
            renaming: None,
            ws: Workspace::default(),
            ws_session: None,
            ws_parked: HashMap::new(),
            attachments: Vec::new(),
            changes_scope: "all",
            changes_search,
            backdrop: Default::default(),
            felt: Default::default(),
            space: Default::default(),
            thumbs: Default::default(),
            pasted: 0,
            models: Vec::new(),
            slash_commands: Vec::new(),
            mention_sel: 0,
            mention_dismissed: None,
            thoughts_open: HashSet::new(),
            #[cfg(feature = "rich")]
            drawn: RefCell::new(HashMap::new()),
            shown: SidebarLayout::default(),
            row_fx: Springs::points(),
            sidebar_hovered: false,
            overview: false,
            overview_fade: Spring::unit(0.0),
            cards: HashMap::new(),
            card_order: Vec::new(),
            overview_cursor: 0,
            overview_focus: cx.focus_handle(),
            focus_overview: false,
            viewport: (1180.0, 780.0),
            refresh_ms: 1000.0 / 60.0,
            prompt_open: Spring::unit(0.0),
            prompt_ghost: Vec::new(),
            ticker: Ticker::default(),
            window_active: true,
            error: None,
            confirm_delete: None,
            answers: HashMap::new(),
            open_on_launch: launch.open,
            panel_on_launch: launch.panel,
            file_on_launch: launch.file,
            overview_on_launch: launch.overview,
            bench: None,
            bench_inactive_frames: 0,
            bench_on_launch: launch.bench_motion,
            bench_from: launch.bench_from,
            have_sessions: false,
            _subscriptions: subscriptions,
            _tasks: vec![pump, clock, beat],
        };
        this.adopt_ui(ui);
        if launch.focus {
            // only the open session: every drawer starts shut.
            for d in &mut this.drawers {
                d.open = false;
                d.w.snap(0.0);
            }
        }
        this
    }

    /// the sidebar as it is currently drawn. for tests.
    #[cfg(test)]
    pub(crate) fn shown_for_test(&self) -> &SidebarLayout {
        &self.shown
    }

    /// what is typed in the message box. for tests.
    #[cfg(test)]
    pub(crate) fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub(crate) fn overview_for_test(&self) -> bool {
        self.overview
    }

    #[cfg(test)]
    pub(crate) fn thought_is_open(&self, id: &ItemId) -> bool {
        self.thoughts_open.contains(id)
    }

    /// how wide the sidebar is right now. for tests.
    #[cfg(test)]
    pub(crate) fn sidebar_width_for_test(&self) -> f32 {
        self.drawer("sidebar").map_or(0.0, |d| d.w.value())
    }

    fn cmd(&self, cmd: Cmd) {
        let _ = self.bridge.cmds.send(cmd);
    }

    // ---- events from the daemon ------------------------------------------------

    fn apply(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Connected => {
                self.model.connection = Connection::Connected;
                if self.models.is_empty() {
                    self.cmd(Cmd::Models);
                }
            }
            UiEvent::Models(models, commands) => {
                self.models = models;
                self.slash_commands = commands;
            }
            UiEvent::Git { session, summary } => self.apply_git(session, summary),
            UiEvent::Diff { session, diff } => self.apply_diff(session, diff, cx),
            UiEvent::Dir { session, path, entries } => self.apply_dir(session, path, entries),
            UiEvent::File { session, file } => self.apply_file(session, file),
            UiEvent::Saved { session, path } => self.apply_saved(session, path),
            UiEvent::Pr { session, status } => self.apply_pr(session, status),
            UiEvent::Down(why) => self.model.connection = Connection::Down(why),
            UiEvent::Sessions(sessions) => {
                self.have_sessions = true;
                self.model.set_sessions(sessions);
                self.reflow_sidebar();
                self.mark_open_seen();
                if let Some(prefix) = self.open_on_launch.take() {
                    let hit = self.model.sessions.iter().find(|s| s.id.to_string().starts_with(&prefix)).map(|s| s.id);
                    match hit {
                        Some(id) => self.open(id, cx),
                        None => self.error = Some(format!("No session starts with {prefix}.")),
                    }
                }
                if std::mem::take(&mut self.overview_on_launch) {
                    self.open_overview(cx);
                }
                if std::mem::take(&mut self.bench_on_launch) {
                    self.start_bench(cx);
                }
            }
            UiEvent::Session(summary) => {
                let (goal, reason) = (summary.goal.clone(), summary.attention.as_ref().map(|a| a.reason));
                let is_open = self.model.open.as_ref().is_some_and(|o| o.id == summary.id);
                // a turn ending is when its changes are worth another look.
                let was_busy = self.model.summary(summary.id).is_some_and(|s| s.run_state.is_busy());
                if is_open && was_busy && !summary.run_state.is_busy() {
                    self.workspace_touched();
                }
                let newly = self.model.update_session(summary);
                self.reflow_sidebar();
                if is_open && self.window_active {
                    self.mark_open_seen();
                } else if newly && let Some(reason) = reason {
                    notify_os(&goal, reason);
                }
            }
            UiEvent::Removed(id) => {
                let was_open = self.model.open.as_ref().is_some_and(|o| o.id == id);
                self.model.remove_session(id);
                self.reflow_sidebar();
                if was_open {
                    self.scroller.update(cx, |s, cx| s.reset(0, cx));
                    self.show_new = true;
                }
            }
            UiEvent::Snapshot(snap) => {
                let open = Open::from_snapshot(*snap);
                let n = open.len();
                self.model.open = Some(open);
                self.answers.clear();
                self.scroller.update(cx, |s, cx| {
                    s.reset(n, cx);
                    s.scroll_to_end(cx);
                });
                self.sync_prompt(false);
                self.mark_open_seen();
                // what the command line asked to see, now that there is a session to show it for.
                let (panel, file) = (self.panel_on_launch.take(), self.file_on_launch.take());
                if panel.is_some() || file.is_some() {
                    self.switch_workspace();
                    if let Some(tab) = panel.as_deref().and_then(|name| Tab::ALL.into_iter().find(|t| t.title().eq_ignore_ascii_case(name))) {
                        self.ws.tab = tab;
                    }
                    if !self.panel_visible() {
                        self.toggle_inspector(cx);
                    }
                    if let Some(path) = file {
                        self.open_file(path, cx);
                        self.refresh_workspace();
                    }
                }
            }
            UiEvent::Item { session, item } => {
                let Some(open) = self.model.open.as_mut().filter(|o| o.id == session) else { return };
                // a tool that has just finished may have changed the folder.
                let touched = matches!(&item.body, ItemBody::Tool { status, .. } if !status.is_open());
                let change = open.upsert(item);
                let n = open.len();
                self.relayout(change, n, cx);
                if touched {
                    self.workspace_touched();
                }
            }
            UiEvent::Delta { session, item_id, offset, text } => {
                let Some(open) = self.model.open.as_mut().filter(|o| o.id == session) else { return };
                let change = open.delta(&item_id, offset, &text);
                let n = open.len();
                self.relayout(change, n, cx);
            }
            UiEvent::Pending { session, pending } => {
                if let Some(open) = self.model.open.as_mut().filter(|o| o.id == session) {
                    self.answers.retain(|id, _| pending.iter().any(|p| &p.request_id == id));
                    open.pending = pending;
                    self.sync_prompt(true);
                }
            }
            UiEvent::Call { call, cmd, args } => self.ui_calls.push((call, cmd, args)),
            UiEvent::Watched(w) => self.ui_watched = w,
            UiEvent::DaemonSettings(d) => self.daemon_settings = Some(d),
            UiEvent::Answer { tag, result } if tag.starts_with("git.") => self.git_answer(tag, result, cx),
            UiEvent::Answer { tag, result } if tag.starts_with("file.") => self.file_answer(tag, result, cx),
            UiEvent::Answer { tag: "palette.files", result } => self.palette_answer(result, cx),
            UiEvent::Answer { tag: "folder.info", result } => {
                if let Some(path) = self.folder_asked.borrow_mut().take() {
                    let git = match result {
                        Ok(tessel_proto::Response::FolderInfo { git_root }) => Some(git_root.is_some()),
                        // an older daemon, or it couldn't look: in place, which always works.
                        _ => Some(false),
                    };
                    self.folder_git.borrow_mut().insert(path, git);
                }
            }
            UiEvent::Answer { .. } => {}
            ev @ (UiEvent::Terms(_) | UiEvent::TermOpened { .. } | UiEvent::TermOutput(..) | UiEvent::TermUpdated(_) | UiEvent::TermRemoved(_)) => self.term_event(ev, cx),
            UiEvent::Created => {
                self.show_new = false;
                self.error = None;
            }
            UiEvent::Error(msg) => self.error = Some(msg),
        }
    }

    /// tell the list exactly what changed so it only re-measures that.
    fn relayout(&mut self, change: Change, len: usize, cx: &mut Context<Self>) {
        self.scroller.update(cx, |s, cx| match change {
            Change::None => {}
            Change::Appended(n) => {
                s.append(n, cx);
            }
            Change::Row(i) => {
                s.remeasure_items(i..i + 1, cx);
            }
            Change::Reset => s.reset(len, cx),
        });
    }

    /// bring the drawn sidebar in line with the model. rows that change
    /// places are made to start from where they were. while the pointer rests
    /// on the list, nothing changes places at all.
    fn reflow_sidebar(&mut self) {
        self.reflow_sidebar_as(self.sidebar_hovered, true);
    }

    /// `hold` keeps rows where they are under the pointer: for what the
    /// daemon changes, never for what you just did yourself.
    pub(crate) fn reflow_sidebar_as(&mut self, hold: bool, glide: bool) {
        let real = SidebarLayout::build(&self.model);
        let next = if hold { self.shown.held(&real) } else { real };
        if next == self.shown {
            return;
        }
        // the first fill, and anything that happens while the window is in
        // the background, just appears: there is nobody to show the change to.
        if glide && !self.shown.entries.is_empty() && self.window_active {
            for (id, dy) in self.shown.moves_to(&next) {
                self.row_fx.shift(id, dy, PANEL);
            }
        }
        self.shown = next;
    }

    /// open or close the prompt bar to match what is pending.
    fn sync_prompt(&mut self, animate: bool) {
        let pending = self.model.open.as_ref().map(|o| o.pending.clone()).unwrap_or_default();
        let target = if pending.is_empty() { 0.0 } else { 1.0 };
        if !pending.is_empty() {
            self.prompt_ghost = pending;
        }
        if animate && self.window_active {
            self.prompt_open.to(target, PANEL);
        } else {
            self.prompt_open.snap(target);
        }
    }

    /// looking at a finished session is what clears its flag. only while the
    /// window is actually in front: a session finishing behind another app
    /// has not been seen.
    fn mark_open_seen(&mut self) {
        if !self.window_active {
            return;
        }
        let Some(s) = self.model.open_summary() else { return };
        if matches!(s.attention.as_ref().map(|a| a.reason), Some(AttentionReason::Finished | AttentionReason::Errored)) {
            self.cmd(Cmd::MarkSeen { session: s.id, seq: s.last_seq });
        }
    }

    // ---- things the user does --------------------------------------------------

    pub fn open(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.show_new = false;
        self.error = None;
        self.confirm_delete = None;
        if self.model.open.as_ref().is_none_or(|o| o.id != id) {
            self.model.open = None;
            self.prompt_open.snap(0.0);
            self.prompt_ghost.clear();
            self.scroller.update(cx, |s, cx| s.reset(0, cx));
            self.cmd(Cmd::Open(Some(id)));
        }
        cx.notify();
    }

    /// the drawer called "sidebar", whatever the layout put in it.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.toggle_drawer("sidebar", cx);
    }

    /// the drawer holding the workspace panel, or else the one called "inspector".
    pub fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        let id = self.panel_drawer.clone().unwrap_or_else(|| "inspector".to_owned());
        self.toggle_drawer(&id, cx);
    }

    /// open or close a finished thought. `row` is where it sits in the transcript.
    fn toggle_thought(&mut self, id: ItemId, row: usize, cx: &mut Context<Self>) {
        if !self.thoughts_open.remove(&id) {
            self.thoughts_open.insert(id);
        }
        self.scroller.update(cx, |s, cx| s.remeasure_items(row..row + 1, cx));
        cx.notify();
    }

    /// send a plan back. whatever is typed in the message box says what to change.
    fn keep_planning(&mut self, request_id: RequestId, window: &mut Window, cx: &mut Context<Self>) {
        let said = self.composer.read(cx).value().trim().to_string();
        let message = if said.is_empty() { "The user wants this plan changed before any work starts. Ask what they would like to be different.".to_owned() } else { said };
        self.answer(request_id, PermissionDecision::Deny { message });
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    /// hide everything but the open session, or bring it all back.
    pub fn toggle_focus_mode(&mut self, cx: &mut Context<Self>) {
        let mut open: Vec<String> = self.drawers.iter().filter(|d| d.open).map(|d| d.id.clone()).collect();
        open.extend(self.ui.panes().iter().filter(|p| p.module != Some(crate::ui::Module::Conversation) && self.pane_open(p)).map(|p| p.id.clone()));
        if open.is_empty() {
            self.toggle_sidebar(cx);
        }
        for id in open {
            self.toggle_drawer(&id, cx);
        }
    }

    // ---- the overview --------------------------------------------------------------

    /// where a session's sidebar row is, in window coordinates. this is where
    /// its card flies from and back to.
    fn row_rect(&self, id: SessionId) -> Rect {
        let top = self.shown.row_tops().get(&id).copied().unwrap_or(0.0);
        // a row scrolled out of view, or a closed sidebar, still gets a
        // sensible place to fly to: clamped to the window's edge.
        let y = (theme::TOOLBAR_H + NAV_H + top).min(self.viewport.1 - crate::model::ROW_H);
        let (size, w) = self.drawer("sidebar").map_or((theme::SIDEBAR_W, 0.0), |d| (d.size, d.w.value()));
        Rect { x: 8.0 - size + w, y, w: size - 16.0, h: crate::model::ROW_H }
    }

    /// the transcript pane, where a chosen card grows to.
    fn content_rect(&self) -> Rect {
        let side = |side: crate::ui::Side| -> f32 { self.drawers.iter().filter(|d| d.side == side).map(|d| d.w.target()).sum() };
        let (x, right) = (side(crate::ui::Side::Left), side(crate::ui::Side::Right));
        Rect { x, y: theme::TOOLBAR_H, w: (self.viewport.0 - x - right).max(200.0), h: (self.viewport.1 - theme::TOOLBAR_H).max(200.0) }
    }

    pub fn toggle_overview(&mut self, cx: &mut Context<Self>) {
        if self.overview {
            self.close_overview(None, cx);
        } else {
            self.open_overview(cx);
        }
    }

    pub fn open_overview(&mut self, cx: &mut Context<Self>) {
        self.overview = true;
        self.focus_overview = true;
        // cards keep the sidebar's order, so each flies the shortest way.
        self.card_order = self.shown.rows().collect();
        let open = self.model.open.as_ref().map(|o| o.id);
        self.overview_cursor = open.and_then(|id| self.card_order.iter().position(|c| *c == id)).unwrap_or(0);
        let cells = grid(self.card_order.len(), self.viewport.0, theme::TOOLBAR_H + 16.0);
        for (id, cell) in self.card_order.clone().into_iter().zip(cells) {
            let from = self.row_rect(id);
            // a card already in flight (the overview was reopened mid-close)
            // carries on from where it is.
            let card = self.cards.entry(id).or_insert_with(|| [Spring::points(from.x), Spring::points(from.y), Spring::points(from.w), Spring::points(from.h)]);
            for (spring, to) in card.iter_mut().zip([cell.x, cell.y, cell.w, cell.h]) {
                spring.to(to, SMOOTH);
            }
        }
        self.cards.retain(|id, _| self.card_order.contains(id));
        self.overview_fade.to(1.0, SMOOTH);
        cx.notify();
    }

    /// leave the overview. with a session chosen, its card grows into the
    /// transcript; every other card flies home to its row.
    pub fn close_overview(&mut self, chosen: Option<SessionId>, cx: &mut Context<Self>) {
        if !self.overview {
            return;
        }
        self.overview = false;
        if let Some(id) = chosen {
            self.open(id, cx);
        }
        let content = self.content_rect();
        for id in self.card_order.clone() {
            let to = if chosen == Some(id) { content } else { self.row_rect(id) };
            if let Some(card) = self.cards.get_mut(&id) {
                for (spring, to) in card.iter_mut().zip([to.x, to.y, to.w, to.h]) {
                    spring.to(to, SMOOTH);
                }
            }
        }
        self.overview_fade.to(0.0, SMOOTH);
        cx.notify();
    }

    pub fn overview_move(&mut self, dx: i32, dy: i32, cx: &mut Context<Self>) {
        if self.overview {
            self.overview_cursor = grid_step(self.overview_cursor, self.card_order.len(), grid_cols(self.viewport.0), dx, dy);
            cx.notify();
        }
    }

    pub fn overview_open_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.card_order.get(self.overview_cursor).copied().filter(|_| self.overview) {
            self.close_overview(Some(id), cx);
        }
    }

    /// jump to the session that has been waiting on you longest.
    pub fn next_attention(&mut self, cx: &mut Context<Self>) {
        let current = self.model.open.as_ref().map(|o| o.id);
        if let Some(next) = self.model.next_attention(current) {
            self.open(next, cx);
        }
    }

    pub fn show_new_session(&mut self, cx: &mut Context<Self>) {
        self.show_new = true;
        cx.notify();
    }

    /// stop the open session's agent. the mac "cancel" key, ⌘-period.
    pub fn stop_agent(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.model.open_summary().filter(|s| s.run_state.is_busy()) {
            self.cmd(Cmd::Interrupt(s.id));
        }
        cx.notify();
    }

    /// allow the oldest pending permission request. questions are skipped:
    /// they need an actual answer.
    pub fn allow_pending(&mut self, cx: &mut Context<Self>) {
        let first = self.model.open.as_ref().and_then(|o| o.pending.iter().find(|p| !p.is_question)).map(|p| p.request_id.clone());
        if let Some(id) = first {
            self.answer(id, PermissionDecision::Allow { updated_input: None });
        }
        cx.notify();
    }

    /// first press arms it, second press deletes. history is not recoverable,
    /// so one stray click must not be enough.
    pub(crate) fn delete(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.confirm_delete == Some(id) {
            self.confirm_delete = None;
            self.cmd(Cmd::Delete { session: id, remove_worktree: false });
        } else {
            self.confirm_delete = Some(id);
        }
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else { return };
        let text = self.composer.read(cx).value().to_string();
        // a screenshot on its own is a message too.
        if text.trim().is_empty() && self.attachments.is_empty() {
            return;
        }
        let attachments = std::mem::take(&mut self.attachments);
        self.cmd(Cmd::Send { session, text, attachments });
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.scroller.update(cx, |s, cx| s.scroll_to_end(cx));
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.new_prompt.read(cx).value().to_string();
        let Some(cwd) = self.new_folder() else {
            self.error = Some("Choose a project for the session to work in.".into());
            cx.notify();
            return;
        };
        let worktree = self.new_worktree && self.folder_is_git(&cwd) != Some(false);
        self.cmd(Cmd::Create { cwd, worktree, prompt });
        self.new_prompt.update(cx, |s, cx| s.set_value("", window, cx));
    }

    fn answer(&mut self, request_id: RequestId, decision: PermissionDecision) {
        if let Some(session) = self.model.open.as_ref().map(|o| o.id) {
            self.answers.remove(&request_id);
            self.cmd(Cmd::Answer { session, request_id, decision });
        }
    }

    /// record one answer to a question; send once every part is answered.
    fn pick(&mut self, request: &PermissionRequest, question: String, label: String, cx: &mut Context<Self>) {
        let picked = self.answers.entry(request.request_id.clone()).or_default();
        picked.insert(question, serde_json::Value::String(label));
        let total = request.input.get("questions").and_then(|q| q.as_array()).map_or(1, Vec::len);
        if picked.len() >= total {
            let mut input = request.input.clone();
            if let Some(obj) = input.as_object_mut() {
                obj.insert("answers".into(), serde_json::Value::Object(picked.clone()));
            }
            self.answer(request.request_id.clone(), PermissionDecision::Allow { updated_input: Some(input) });
        }
        cx.notify();
    }

    // ---- measuring motion --------------------------------------------------------

    /// make one session start or stop needing you, so the queue has to rearrange.
    fn bench_shuffle(&mut self, cx: &mut Context<Self>) {
        let n = self.model.sessions.len();
        if n == 0 {
            return;
        }
        let turn = self.bench.as_ref().map_or(0, |b| b.1.len());
        let s = &mut self.model.sessions[turn % n];
        s.attention = match s.attention.take() {
            Some(_) => None,
            None => Some(tessel_proto::Attention { reason: AttentionReason::Finished, since_ms: now_ms(), seq: s.last_seq }),
        };
        self.reflow_sidebar();
        cx.notify();
    }

    /// run each transition, some interrupted mid-flight, then report and quit.
    fn start_bench(&mut self, cx: &mut Context<Self>) {
        type Step = (&'static str, fn(&mut Shell, &mut Context<Shell>), u64);
        const SCRIPT: &[Step] = &[
            ("overview", Shell::toggle_overview, 900),
            ("overview", Shell::toggle_overview, 250),
            ("overview", Shell::toggle_overview, 900),
            ("overview", Shell::toggle_overview, 900),
            ("sidebar", Shell::toggle_sidebar, 700),
            ("sidebar", Shell::toggle_sidebar, 180),
            ("sidebar", Shell::toggle_sidebar, 700),
            ("sidebar", Shell::toggle_sidebar, 700),
            ("inspector", Shell::toggle_inspector, 600),
            ("inspector", Shell::toggle_inspector, 600),
            ("queue", Shell::bench_shuffle, 500),
            ("queue", Shell::bench_shuffle, 120),
            ("queue", Shell::bench_shuffle, 500),
            ("queue", Shell::bench_shuffle, 500),
        ];
        self.bench = Some(("warmup", Vec::new()));
        // rows only travel when nothing is resting on them and the window is in front.
        self.sidebar_hovered = false;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(900)).await;
            for (phase, action, ms) in SCRIPT {
                let ok = this.update(cx, |shell, cx| {
                    if let Some(b) = shell.bench.as_mut() {
                        b.0 = phase;
                    }
                    action(shell, cx);
                });
                if ok.is_err() {
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(*ms)).await;
            }
            let _ = this.update(cx, |shell, cx| {
                shell.report_bench();
                cx.quit();
            });
        });
        self._tasks.push(task);
    }

    fn report_bench(&self) {
        let Some((_, frames)) = &self.bench else { return };
        let budget = self.refresh_ms;
        println!("display: {budget:.2} ms per frame ({:.0} Hz)", 1000.0 / budget);
        println!("{:<10} {:>7} {:>8} {:>8} {:>8} {:>8} {:>14}", "transition", "frames", "p50 ms", "p95 ms", "p99 ms", "max ms", "late ms per s");
        let mut slow = 0;
        let mut total = 0;
        for phase in ["overview", "sidebar", "inspector", "queue"] {
            // an interval counts only when both of its frames were mid-animation:
            // the pause between two transitions is not a slow frame.
            let mut gaps: Vec<f32> = frames
                .windows(2)
                .filter(|w| w[0].0 == phase && w[1].0 == phase && w[0].2 && w[1].2)
                .map(|w| (w[1].1 - w[0].1).as_secs_f32() * 1000.0)
                .collect();
            if gaps.is_empty() {
                println!("{phase:<10} no frames recorded");
                continue;
            }
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |p: f32| gaps[((gaps.len() - 1) as f32 * p) as usize];
            let seconds: f32 = gaps.iter().sum::<f32>() / 1000.0;
            // apple's measure: how late frames were, per second of animation.
            let late: f32 = gaps.iter().map(|g| g - budget).filter(|l| *l > budget * 0.5).sum();
            slow += gaps.iter().filter(|g| **g > budget * 1.5).count();
            total += gaps.len();
            println!("{phase:<10} {:>7} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>14.1}", gaps.len(), pct(0.5), pct(0.95), pct(0.99), gaps[gaps.len() - 1], (late / seconds.max(0.001)).max(0.0));
        }
        println!("frames more than half a frame late: {slow} of {total}. apple's bar for good is under 10 ms late per second.");
        if self.bench_inactive_frames > 0 {
            println!(
                "NOT A VALID RUN: the window was in the background for {} of {} frames. a background window is held to 30 frames a second, so these numbers measure that limit and not the app. bring the window to the front and run again.",
                self.bench_inactive_frames,
                frames.len()
            );
        }
    }

    // ---- drawing -----------------------------------------------------------------

    /// every session, the ones that need you first. fills the height it is given and scrolls.
    pub(crate) fn sessions_list(&self, now: Millis, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = self.palette.get();
        let open = self.model.open.as_ref().map(|o| o.id);

        let mut list = v_flex().id("sidebar-list").flex_1().overflow_y_scroll().pb_2().on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.sidebar_hovered = *hovered;
            if !*hovered {
                // the pointer has left: now things may take their real places.
                this.reflow_sidebar();
            }
            cx.notify();
        }));
        if self.shown.entries.is_empty() && self.have_sessions {
            list = list.child(div().px_4().pt_3().set(Type::Subheadline).text_color(p.secondary).child("No sessions yet."));
        }
        for entry in &self.shown.entries {
            match entry {
                Entry::Header(title) => {
                    list = list.child(self.project_header(title, cx));
                }
                Entry::Row(id) => {
                    let Some(s) = self.model.summary(*id) else { continue };
                    let in_queue = s.attention.is_some();
                    let dy = self.row_fx.value(id);
                    list = list.child(fly(point(px(0.), px(dy)), self.session_row(s, open == Some(s.id), in_queue, now, cx)));
                }
                Entry::Sub(id, task) => {
                    let Some(s) = self.model.summary(*id) else { continue };
                    // a line held in place under the pointer may outlive what it described.
                    let running = s.tasks.iter().find(|t| &t.id == task);
                    let text = running.map_or_else(|| "Finished".to_owned(), |t| sentence(&t.description));
                    let kind = if running.is_some_and(|t| t.background) { "In the background" } else { "Subagent" };
                    let sid = *id;
                    let dy = self.row_fx.value(id);
                    list = list.child(fly(
                        point(px(0.), px(dy)),
                        // a subagent hangs off its session: a guide line down from the
                        // session's icon, a bot, and what it was asked to do.
                        h_flex()
                            .id(ElementId::from(SharedString::from(format!("sub-{sid}-{task}"))))
                            .h(px(crate::model::SUB_H))
                            .ml(px(SESSION_INDENT + 8.))
                            .mr_2()
                            .pr_2()
                            .gap_2()
                            .rounded(px(theme::R_TILE))
                            .hover(|d| d.bg(p.hover))
                            .on_click(cx.listener(move |this, _, _, cx| this.open(sid, cx)))
                            .child(div().w(px(1.)).h_full().flex_shrink_0().bg(p.separator))
                            .child(gpui_kit::component::Icon::empty().path(if kind == "Subagent" { "icons/bot.svg" } else { "icons/hourglass.svg" }).size(theme::z(13.)).text_color(if running.is_some() { p.secondary } else { p.tertiary }))
                            .child(div().flex_1().overflow_hidden().set(Type::Callout).text_color(p.secondary).truncate().child(text.clone()))
                            .test_support()
                            .aria_label(SharedString::from(format!("{kind}: {text}"))),
                    ));
                }
            }
        }

        // the list scrolls; above it, the two places you always go.
        let needing = self.model.attention_queue().len();
        let nav_row = |id: &'static str, icon: &'static str, label: &'static str, trailing: AnyElement| {
            h_flex()
                .id(id)
                .h(px(NAV_ROW_H))
                .mx_2()
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(theme::R_TILE))
                .hover(|d| d.bg(p.hover))
                .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(theme::z(15.)).text_color(p.secondary))
                .child(div().flex_1().set(Type::Body).text_color(p.label).child(label))
                .child(trailing)
        };
        let nav = v_flex()
            .flex_shrink_0()
            .pb(px(NAV_PAD))
            .child(nav_row("nav-new", "square-pen", "New session", theme::keycaps(&p, "⌘ N").into_any_element()).on_click(cx.listener(|this, _, _, cx| this.show_new_session(cx))))
            .child(
                nav_row(
                    "nav-all",
                    "layout-grid",
                    "All sessions",
                    theme::count_pill(&p, if needing > 0 { format!("{needing} need you") } else { self.model.sessions.len().to_string() }, needing > 0).into_any_element(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_overview(cx))),
            );
        v_flex().id("sidebar-sessions").flex_1().min_h_0().child(nav).child(list)
    }

    fn session_row(&self, s: &SessionSummary, selected: bool, in_queue: bool, now: Millis, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let state = state_mark(s, &p);
        // the time sits on the first line, right aligned, so a long activity
        // line can never push it out of view.
        let (second, time) = row_lines(s, in_queue, now);
        let sid = s.id;
        let label: SharedString = format!("{}, {second}", s.goal).into();
        let group = SharedString::from(format!("session-row-{sid}"));
        let menu = self.session_menu(s, group.clone(), cx);
        let in_workspace: Vec<&SessionSummary> = self.model.sessions.iter().filter(|o| o.cwd == s.cwd).collect();
        let tabs = in_workspace.len();
        let workspace_title = in_workspace.iter().min_by_key(|o| (o.created_ms, o.id)).map_or_else(|| s.goal.clone(), |o| o.goal.clone());
        let hover_group = group.clone();
        h_flex()
            .group(group)
            .id(ElementId::from(sid.0))
            .test_support()
            .aria_label(label)
            .relative()
            .h(px(crate::model::ROW_H - 3.))
            .my(px(1.5))
            .mx_2()
            .pl(px(SESSION_INDENT - 8.))
            .pr_2()
            .gap_2()
            .rounded(px(theme::R_TILE))
            .when(selected, |d| d.bg(p.selection))
            .when(!selected, |d| d.hover(|d| d.bg(p.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.open(sid, cx)))
            // at rest a session shows what it is (a branch of its own, or a
            // conversation); anything else shows its state, which is what matters then.
            .child(match state {
                Mark::Ring(_) => div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(gpui_kit::component::Icon::empty().path(if s.worktree.is_some() { "icons/git-branch.svg" } else { "icons/message-square.svg" }).size(theme::z(14.)).text_color(p.tertiary))
                    .into_any_element(),
                m => mark(m).into_any_element(),
            })
            .child(
                v_flex()
                    .flex_1()
                    .overflow_hidden()
                    .gap(px(1.))
                    .child(
                        h_flex()
                            .gap_2()
                            // the workspace: what it was made for (its first tab), and how many tabs it has.
                            .child(div().flex_1().overflow_hidden().set(Type::Body).text_color(p.label).truncate().child(workspace_title))
                            .when(tabs > 1, |d| d.child(theme::count_pill(&p, tabs.to_string(), false)))
                            // the time, and on hover the menu in its place: one slot at the
                            // end of the first line, wherever the line below ends.
                            // the time, which makes way for the menu on hover.
                            .children(time.map(|t| div().flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).group_hover(hover_group.clone(), |d| d.invisible()).child(t))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().overflow_hidden().set(Type::Subheadline).text_color(p.secondary).truncate().child(second))
                            .when(self.sessions_trace && !s.trace.is_empty(), |d| d.child(trace(&s.trace, p.tertiary, 56., 11.))),
                    ),
            )
            // the menu: centred on the row, inside its corner, never past it.
            .child(h_flex().absolute().right(px(6.)).top_0().bottom_0().items_center().child(menu))
    }

    fn overview_layer(&self, now: Millis, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let fade = self.overview_fade.value().clamp(0.0, 1.0);
        let needing = self.model.attention_queue().len();
        let title = match needing {
            0 => "All Sessions".to_owned(),
            1 => "All Sessions · 1 needs you".to_owned(),
            n => format!("All Sessions · {n} need you"),
        };
        let mut layer = div()
            .id("overview")
            .absolute()
            .inset_0()
            // while it is up, nothing underneath can be clicked by accident.
            .occlude()
            .track_focus(&self.overview_focus)
            .key_context("Overview")
            .on_action(cx.listener(|this, _: &crate::CloseOverview, _, cx| this.close_overview(None, cx)))
            .on_action(cx.listener(|this, _: &crate::OverviewOpen, _, cx| this.overview_open_cursor(cx)))
            .on_action(cx.listener(|this, _: &crate::OverviewLeft, _, cx| this.overview_move(-1, 0, cx)))
            .on_action(cx.listener(|this, _: &crate::OverviewRight, _, cx| this.overview_move(1, 0, cx)))
            .on_action(cx.listener(|this, _: &crate::OverviewUp, _, cx| this.overview_move(0, -1, cx)))
            .on_action(cx.listener(|this, _: &crate::OverviewDown, _, cx| this.overview_move(0, 1, cx)))
            .bg(p.window.opacity(fade))
            .child(
                self.drag_region("overview-titlebar", cx)
                    .h(px(theme::TOOLBAR_H))
                    .pl(px(theme::WINDOW_BUTTONS_W + 12.))
                    .flex()
                    .items_center()
                    .opacity(fade)
                    .pr(px(12.))
                    .set(Type::Title)
                    .text_color(p.label)
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    // always a way back, not only escape.
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(px(6.))
                            .child(theme::keycaps(&p, "esc"))
                            .child(
                                theme::button(&p, "overview-close", "Back", Some("arrow-left"), theme::ButtonKind::Secondary)
                                    .test_support()
                                    .aria_label("Back")
                                    .on_click(cx.listener(|this, _, _, cx| this.close_overview(None, cx))),
                            ),
                    ),
            );
        // nothing to show: say so, and offer the way to make one.
        if self.card_order.is_empty() {
            layer = layer.child(
                v_flex()
                    .id("overview-empty")
                    .test_support()
                    .absolute()
                    .top(px(theme::TOOLBAR_H))
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .items_center()
                    .justify_center()
                    .gap(theme::z(10.))
                    .opacity(fade)
                    .child(gpui_kit::component::Icon::empty().path("icons/layout-grid.svg").size(theme::z(24.)).text_color(p.tertiary))
                    .child(div().set(Type::Headline).text_color(p.label).child("No sessions yet"))
                    .child(div().set(Type::Callout).text_color(p.secondary).child("Every session you start shows here, the ones that need you first."))
                    .child(
                        theme::button(&p, "overview-new", "New session", Some("square-pen"), theme::ButtonKind::Primary).on_click(cx.listener(|this, _, window, cx| {
                            this.close_overview(None, cx);
                            if let Err(e) = this.exec("session.new", &serde_json::json!({}), window, cx) {
                                this.error = Some(e);
                            }
                        })),
                    ),
            );
        }
        for (ix, id) in self.card_order.iter().enumerate() {
            let (Some(card), Some(s)) = (self.cards.get(id), self.model.summary(*id)) else { continue };
            let [x, y, w, h] = [card[0].value(), card[1].value(), card[2].value(), card[3].value()];
            // the card's contents are laid out once, at the size the card is
            // heading for, and only ever clipped by the moving frame. nothing
            // re-wraps mid-flight.
            let inner_w = if self.overview { card[2].target() } else { grid(1, self.viewport.0, 0.0)[0].w.max(card[2].value()) };
            let in_queue = s.attention.is_some();
            let (second, time) = row_lines(s, in_queue, now);
            let focused = self.overview && ix == self.overview_cursor;
            let sid = *id;
            layer = layer.child(
                div()
                    .id(ElementId::from(SharedString::from(format!("card-{sid}"))))
                    .test_support()
                    .aria_label(SharedString::from(format!("{}, {second}", s.goal)))
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w.max(1.0)))
                    .h(px(h.max(1.0)))
                    .overflow_hidden()
                    .rounded(px(theme::R_CARD))
                    .border_1()
                    .border_color(if focused { p.accent } else { p.separator })
                    .bg(p.sidebar)
                    .opacity(fade)
                    .on_click(cx.listener(move |this, _, _, cx| this.close_overview(Some(sid), cx)))
                    .child(
                        v_flex()
                            .w(px(inner_w))
                            .h(px(crate::model::CARD_H))
                            .p_3()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(mark(state_mark(s, &p)))
                                    .child(div().flex_1().overflow_hidden().set(Type::Headline).text_color(p.label).truncate().child(s.goal.clone()))
                                    .children(time.map(|t| div().flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).child(t))),
                            )
                            .child(div().pl(px(22.)).set(Type::Callout).text_color(p.secondary).line_clamp(2).child(second))
                            .child(div().flex_1())
                            .child(
                                h_flex()
                                    .pl(px(22.))
                                    .gap_2()
                                    .items_end()
                                    .child(div().flex_1().overflow_hidden().set(Type::Subheadline).text_color(p.tertiary).truncate().child(project_name(s)))
                                    .when(!s.trace.is_empty(), |d| d.child(trace(&s.trace, p.secondary, 96., 16.))),
                            ),
                    ),
            );
        }
        layer
    }

    /// a stretch of title bar: dragging it moves the window, double-clicking zooms.
    fn drag_region(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        div().id(id).on_double_click(|_, window, _| window.titlebar_double_click()).on_mouse_move(self.moves_window(cx))
    }

    /// what a stretch of title bar does with a held mouse: moves the window,
    /// unless the press was for something in the window (a pane by its grip,
    /// a divider, a row) and it's that being carried.
    pub(crate) fn moves_window(&self, cx: &mut Context<Self>) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        cx.listener(|this, ev: &MouseMoveEvent, window, _| {
            if ev.pressed_button == Some(MouseButton::Left) && !this.carrying() {
                window.start_window_move();
            }
        })
    }

    /// something in the window is being dragged.
    pub(crate) fn carrying(&self) -> bool {
        self.pane_move.is_some() || self.pane_drag.is_some() || self.drag.is_some()
    }

    /// whether a folder is in a git repository: asked of the daemon the first
    /// time, `None` until it has answered.
    pub(crate) fn folder_is_git(&self, folder: &str) -> Option<bool> {
        if let Some(known) = self.folder_git.borrow().get(folder) {
            return *known;
        }
        if self.folder_asked.borrow().is_none() {
            *self.folder_asked.borrow_mut() = Some(folder.to_owned());
            self.folder_git.borrow_mut().insert(folder.to_owned(), None);
            self.cmd(Cmd::Ask { tag: "folder.info", req: tessel_proto::Request::FolderInfo { path: folder.to_owned() } });
        }
        None
    }

    pub(crate) fn new_session_form(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let folder = self.new_folder();
        let plain = folder.as_deref().and_then(|f| self.folder_is_git(f)) == Some(false);
        let worktree = self.new_worktree && !plain;
        let picker = self.project_picker(cx);
        let segment = |id: &'static str, label: &'static str, on: bool, set: bool| {
            let disabled = set && plain;
            theme::segment(&p, id, label, on)
                .test_support()
                .aria_label(SharedString::from(format!("{label}{}", if on { ", selected" } else if disabled { ", not available here" } else { "" })))
                .when(disabled, |d| d.opacity(0.4))
                .when(!disabled, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        this.new_worktree = set;
                        cx.notify();
                    }))
                })
        };
        let card = v_flex()
            .w_full()
            .p_3()
            .gap_3()
            .rounded(px(theme::R_CARD))
            .border_1()
            .border_color(p.separator)
            .bg(p.raised)
            .shadow_md()
            .child(Input::new(&self.new_prompt).id("new-prompt").appearance(false))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(picker)
                    .child(theme::tray(&p).child(segment("in-place", "In place", !worktree, false)).child(segment("worktree-toggle", "New worktree", worktree, true)))
                    .child(div().flex_1().min_w(px(0.)))
                    .child(
                        theme::button(&p, "create", "Start", None, theme::ButtonKind::Primary)
                            .test_support()
                            .aria_label("Start")
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            );
        v_flex().size_full().items_center().justify_center().px_4().pb(px(48.)).child(
            v_flex()
                .w_full()
                .max_w(px(640.))
                // what the choice means, inside the card: it stays readable over the backdrop.
                .child(card.child(div().px_1().set(Type::Subheadline).text_color(p.secondary).child(match (&folder, worktree) {
                    (None, _) => "Pick a project, or open a new one, for it to work in.".to_owned(),
                    (Some(f), false) if plain => format!("Works directly in {}. It isn't a git repository, so it can't have a workspace of its own.", projects::tilde(f)),
                    (Some(_), true) => "A workspace of its own: a new branch and folder. Your checkout is untouched.".to_owned(),
                    (Some(f), false) => format!("Works directly in {}.", projects::tilde(f)),
                }))),
        )
    }

    fn row(&self, ix: usize, me: &Entity<Self>, cx: &App) -> AnyElement {
        let _ = cx;
        let Some((item, depth)) = self.model.open.as_ref().and_then(|o| o.row(ix)) else {
            return div().into_any_element();
        };
        let p = self.palette.get();
        // `None` is an item with nothing to show (a thinking block with no
        // text, say). it gets no row padding either, so it leaves no gap.
        let body: Option<AnyElement> = match &item.body {
            // a subagent's report comes back wrapped as a message "from the
            // user". it isn't one: show the report, quietly, as what it is.
            // what you did to a surface goes to the agent as a message; here it's a quiet line.
            ItemBody::User { text, .. } if text.starts_with("<surface-event") => Some(
                h_flex()
                    .gap_2()
                    .set(Type::Callout)
                    .text_color(p.tertiary)
                    .child(gpui_kit::component::Icon::empty().path("icons/mouse-pointer-click.svg").size(px(13.)))
                    .child(surface_event_line(text))
                    .into_any_element(),
            ),
            // a pane surface is drawn in the Surfaces pane when one is open, here when not: never dropped.
            ItemBody::Surface { region, .. } if region == "pane" && self.surfaces_docked => None,
            ItemBody::Surface { surface, node, .. } => self.surface_el(surface, node, cx),
            // tessel's own tools: what they did (a surface, the window changing) is the record.
            // ("mcp__photon__" is how they were called before the rename.)
            ItemBody::Tool { tool, status, .. } if (tool.starts_with("mcp__tessel__") || tool.starts_with("mcp__photon__")) && *status != ToolStatus::Failed => None,
            ItemBody::User { text, .. } if let Some(report) = handback(text) => Some(
                v_flex()
                    .gap(px(6.))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .set(Type::Callout)
                            .child(mark(Mark::Check(p.finished)))
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.label).child("Subagent"))
                            .child(div().text_color(p.secondary).child("reported back")),
                    )
                    .child(div().ml(px(6.)).pl(px(14.)).border_l_1().border_color(p.separator).set(Type::Callout).text_color(p.secondary).line_clamp(6).child(report))
                    .into_any_element(),
            ),
            ItemBody::User { text, attachments } => {
                // it hugs what you wrote; a full-width bar would read as a heading.
                let bubble = div().max_w(relative(0.8)).px(px(16.)).py(px(10.)).rounded(px(18.)).bg(p.fill).set(Type::Prose).text_color(p.label).child(text.clone());
                let sent = (!attachments.is_empty()).then(|| div().px_1().set(Type::Subheadline).text_color(p.secondary).child(format!("With {}", attachments.join(", "))));
                let col = if self.transcript_opts.user_left { v_flex().items_start() } else { v_flex().items_end() };
                Some(col.gap_1().when(!text.trim().is_empty(), |d| d.child(bubble)).children(sent).into_any_element())
            }
            ItemBody::Text { text, .. } if text.is_empty() => None,
            // a reply with a diagram or display maths in it is drawn piece by piece.
            #[cfg(feature = "rich")]
            ItemBody::Text { text, .. } if crate::rich::has_rich(text) => {
                let mut col = v_flex().gap_2().set(Type::Prose).text_color(p.label);
                for (n, seg) in crate::rich::split(text).into_iter().enumerate() {
                    col = match seg {
                        crate::rich::Seg::Prose(md) => col.child(TextView::markdown(ElementId::from(SharedString::from(format!("md-{}-{n}", item.id))), md).selectable(true).style(theme::prose(&p))),
                        crate::rich::Seg::Mermaid(src) => col.child(self.picture(true, src)),
                        crate::rich::Seg::Math(tex) => col.child(self.picture(false, tex)),
                    };
                }
                Some(col.into_any_element())
            }
            ItemBody::Text { text, .. } => {
                Some(div().set(Type::Prose).text_color(p.label).child(TextView::markdown(ElementId::from(SharedString::from(format!("md-{}", item.id))), text.clone()).selectable(true).style(theme::prose(&p))).into_any_element())
            }
            ItemBody::Thinking { text, streaming: true } if text.is_empty() => Some(div().set(Type::Callout).text_color(p.secondary).child("Thinking…").into_any_element()),
            ItemBody::Thinking { text, .. } if text.is_empty() => None,
            ItemBody::Thinking { streaming: true, .. } if self.transcript_opts.thoughts == Show::Hidden => {
                Some(div().set(Type::Callout).text_color(p.secondary).child("Thinking…").into_any_element())
            }
            ItemBody::Thinking { .. } if self.transcript_opts.thoughts == Show::Hidden => None,
            // the agent's reasoning: shown in full while it is being written,
            // then folded to its one line, opening again on a click.
            ItemBody::Thinking { text, streaming } => {
                let open = *streaming || self.thoughts_open.contains(&item.id);
                let (me, id) = (me.clone(), item.id.clone());
                Some(
                    v_flex()
                        .id(ElementId::from(SharedString::from(format!("thought-{}", item.id))))
                        .test_support()
                        .aria_label(if open { "Thought, showing all of it" } else { "Thought, folded" })
                        .gap(px(6.))
                        .when(!*streaming, |d| d.cursor_pointer().on_click(move |_, _, cx| me.update(cx, |this, cx| this.toggle_thought(id.clone(), ix, cx))))
                        .child(
                            h_flex()
                                .gap(px(4.))
                                .set(Type::Callout)
                                .italic()
                                .text_color(p.tertiary)
                                .child(if *streaming { "Thinking…" } else { "Thought" })
                                .when(!*streaming, |d| d.child(gpui_kit::component::Icon::empty().path(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }).size(px(12.)).text_color(p.tertiary))),
                        )
                        // folded, a thought is its one line; open, all of it.
                        .when(open, |d| d.child(div().pl(px(14.)).set(Type::Callout).italic().text_color(p.secondary).child(text.trim_end().to_owned())))
                        .into_any_element(),
                )
            }
            // a plan stays in the transcript as a document, not a one-line tool call:
            // it is what the rest of the session is measured against.
            ItemBody::Tool { tool, input, status, .. } if tool == "ExitPlanMode" && input.get("plan").and_then(|v| v.as_str()).is_some_and(|t| !t.trim().is_empty()) => {
                let plan = input.get("plan").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                let title = match status {
                    ToolStatus::AwaitingPermission => "Plan · Waiting for You",
                    ToolStatus::Ok => "Plan · Approved",
                    ToolStatus::Denied | ToolStatus::Failed => "Plan · Sent Back",
                    _ => "Plan",
                };
                Some(
                    v_flex()
                        .p_3()
                        .gap_2()
                        .rounded(px(theme::R_CARD))
                        .border_1()
                        .border_color(p.separator)
                        .child(div().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(title))
                        .child(div().set(Type::Prose).text_color(p.label).child(TextView::markdown(ElementId::from(SharedString::from(format!("plan-{}", item.id))), plan).selectable(true).style(theme::prose(&p))))
                        .into_any_element(),
                )
            }
            ItemBody::Tool { status, .. }
                if match self.transcript_opts.tools {
                    Show::Shown => false,
                    Show::Failed => !matches!(status, ToolStatus::Failed | ToolStatus::AwaitingPermission),
                    Show::Hidden => *status != ToolStatus::AwaitingPermission,
                } =>
            {
                None
            }
            ItemBody::Tool { tool, input, status, output, started_ms, finished_ms } => {
                // how long it ran, not how long ago it was asked for.
                let ran = started_ms.zip(*finished_ms).map(|(s, f)| f.saturating_sub(s));
                Some(tool_row(tool, input, *status, output.as_deref(), ran, &p).into_any_element())
            }
            ItemBody::Turn { started_ms, finished_ms, duration_ms, cost_usd, model, fast, usage, files } => {
                Some(self.turn_footer(ix, &item.id, *started_ms, *finished_ms, *duration_ms, *cost_usd, model.as_deref(), *fast, *usage, files, me))
            }
            ItemBody::Notice { level, text } => {
                let color = if *level == NoticeLevel::Error { p.failed } else { p.secondary };
                Some(h_flex().justify_center().set(Type::Subheadline).text_color(color).child(sentence(text)).into_any_element())
            }
            ItemBody::Unknown => None,
        };
        let Some(body) = body else { return div().into_any_element() };
        // what was said can be copied: a button that appears over it on hover.
        let copy_text = match &item.body {
            ItemBody::Text { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            ItemBody::User { text, .. } if handback(text).is_none() && !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        };
        let body = match copy_text {
            None => body,
            Some(text) => {
                let group = SharedString::from(format!("row-{}", item.id));
                div()
                    .relative()
                    .group(group.clone())
                    .child(body)
                    .child(
                        div().absolute().top(px(-6.)).right(px(-6.)).invisible().group_hover(group, |s| s.visible()).child(
                            Button::new(("copy", ix))
                                .ghost()
                                .xsmall()
                                .icon(gpui_kit::component::Icon::empty().path("icons/copy.svg"))
                                .tooltip("Copy")
                                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))),
                        ),
                    )
                    .into_any_element()
            }
        };
        // work done inside a subagent hangs off its call, one step in per level.
        let row = if depth > 0 {
            let indent = px(18. * f32::from(depth.min(6)));
            div().pl(indent).py_1().child(div().pl_3().border_l_2().border_color(p.separator).child(body))
        } else {
            // a turn starts with what you said: room above it. the agent's
            // words breathe; its tool calls sit close, as one run of work.
            let (top, bottom) = match &item.body {
                ItemBody::User { text, .. } if handback(text).is_none() && !text.starts_with("<surface-event") => (if ix == 0 { 8. } else { 28. }, 10.),
                ItemBody::Text { .. } => (8., 8.),
                ItemBody::Thinking { .. } => (6., 6.),
                ItemBody::Tool { .. } => (3., 3.),
                ItemBody::Turn { .. } => (2., 6.),
                _ => (8., 8.),
            };
            div().pt(px(top)).pb(px(bottom)).child(body)
        };
        // the list spans the pane, so its scrollbar is at the pane's edge;
        // each row keeps to the reading column in the middle of it.
        let t = self.transcript_opts;
        h_flex().w_full().justify_center().child(row.w_full().max_w(px(t.width)).px(px(t.gutter))).into_any_element()
    }

    /// the end of a turn: how long it ran and when, the reply to copy, and the
    /// files it changed. its time opens a card of what it cost and took.
    #[allow(clippy::too_many_arguments)]
    fn turn_footer(
        &self,
        ix: usize,
        id: &ItemId,
        started_ms: Millis,
        finished_ms: Millis,
        duration_ms: Option<u64>,
        cost_usd: Option<f64>,
        model: Option<&str>,
        fast: bool,
        usage: Option<tessel_proto::TurnUsage>,
        files: &[tessel_proto::TurnFile],
        me: &Entity<Self>,
    ) -> AnyElement {
        let p = self.palette.get();
        let took = ago(0, duration_ms.unwrap_or(finished_ms.saturating_sub(started_ms)));
        let clock = |ms: Millis, date: bool| {
            use chrono::TimeZone as _;
            chrono::Local.timestamp_millis_opt(ms as i64).single().map_or(String::new(), |t| t.format(if date { "%b %-d, %Y, %-I:%M %p" } else { "%-I:%M %p" }).to_string())
        };
        // the card: what answered, from when to when, and what it took.
        let mut rows: Vec<(String, String)> = vec![("Fast mode".into(), if fast { "On".into() } else { "Off".into() })];
        if let Some(u) = usage {
            for (k, v) in [("Input", u.input), ("Output", u.output), ("Cache read", u.cache_read), ("Cache write", u.cache_write)] {
                rows.push((k.into(), panel::thousands(v)));
            }
        }
        if let Some(c) = cost_usd {
            rows.push(("Cost".into(), format!("${c:.2}")));
        }
        let tip = TurnTip {
            p,
            title: model.map_or_else(|| "The agent".to_owned(), |m| self.model_name(m)),
            when: format!("{} → {}", clock(started_ms, true), clock(finished_ms, true)),
            rows,
        };
        // the reply, all of it: every piece of text since the message that began the turn.
        let reply = self.turn_text(ix);
        let mut row = h_flex()
            .gap(px(10.))
            .set(Type::Subheadline)
            .text_color(p.tertiary)
            .child(
                div()
                    .id(SharedString::from(format!("turn-{id}-usage")))
                    .test_support()
                    .cursor_default()
                    .tooltip(move |_, cx| cx.new(|_| tip.clone()).into())
                    .child(format!("{took} · {}", clock(finished_ms, false))),
            )
            .child(
                Button::new(SharedString::from(format!("turn-{id}-copy")))
                    .ghost()
                    .xsmall()
                    .icon(gpui_kit::component::Icon::empty().path("icons/copy.svg"))
                    .tooltip("Copy the reply")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(reply.clone()))),
            )
            .child(self.turn_menu(id, me));
        // the files it changed: two by name, then the rest as one.
        let chip = |id: SharedString, name: String, added: u32, removed: u32| {
            h_flex()
                .id(id)
                .h(px(22.))
                .px(px(7.))
                .gap(px(6.))
                .rounded(px(theme::R_TILE - 2.))
                .border_1()
                .border_color(p.separator)
                .cursor_pointer()
                .hover(move |d| d.bg(p.hover))
                .child(gpui_kit::component::Icon::empty().path("icons/file.svg").size(px(12.)).text_color(p.secondary))
                .child(div().max_w(px(180.)).truncate().text_color(p.label).child(name))
                .child(div().set(Type::Code).text_color(p.finished).child(format!("+{added}")))
                .child(div().set(Type::Code).text_color(p.failed).child(format!("-{removed}")))
        };
        for (i, f) in files.iter().take(2).enumerate() {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_owned();
            let (me, path) = (me.clone(), f.path.clone());
            row = row.child(chip(SharedString::from(format!("turn-{id}-file-{i}")), name, f.added, f.removed).tooltip({
                let path = f.path.clone();
                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(path.clone()).build(w, cx)
            }).on_click(move |_, window, cx| {
                me.update(cx, |this, cx| {
                    let _ = this.exec("files.open", &serde_json::json!({"path": path}), window, cx);
                });
            }).test_support());
        }
        if files.len() > 2 {
            let rest = &files[2..];
            let (a, r) = rest.iter().fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
            let me = me.clone();
            row = row.child(chip(SharedString::from(format!("turn-{id}-file-more")), format!("+{} more", rest.len()), a, r).on_click(move |_, window, cx| {
                me.update(cx, |this, cx| {
                    let _ = this.exec("workspace.tab", &serde_json::json!({"tab": "changes"}), window, cx);
                });
            }).test_support());
        }
        row.into_any_element()
    }

    #[cfg(test)]
    pub(crate) fn turn_text_for_test(&self, ix: usize) -> String {
        self.turn_text(ix)
    }

    /// what can be done from the end of a turn: carry the conversation on elsewhere.
    fn turn_menu(&self, id: &ItemId, me: &Entity<Self>) -> AnyElement {
        let me = me.downgrade();
        gpui_kit::component::popover::Popover::new(SharedString::from(format!("turn-{id}-menu")))
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(Button::new(SharedString::from(format!("turn-{id}-more"))).ghost().xsmall().icon(gpui_kit::component::Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let p = shell.read(cx).palette.get();
                let fork = |workspace: bool| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| this.run_key("session.fork", serde_json::json!({"workspace": workspace}), window, cx));
                    }
                };
                theme::float_list(&p)
                    .w(px(240.))
                    .child(theme::float_item(&p, "turn-fork-tab", Some("message-square-plus"), "Fork to new tab", Some("Same workspace, the conversation so far"), false).test_support().on_click(fork(false)))
                    .child(theme::float_item(&p, "turn-fork-workspace", Some("git-fork"), "Fork to new workspace", Some("A new branch from here, the conversation so far"), false).test_support().on_click(fork(true)))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// the text of the turn ending at row `ix`: everything the agent said since
    /// the message that started it.
    fn turn_text(&self, ix: usize) -> String {
        let Some(open) = self.model.open.as_ref() else { return String::new() };
        let mut parts = Vec::new();
        for j in (0..ix).rev() {
            let Some((item, depth)) = open.row(j) else { break };
            match &item.body {
                ItemBody::User { text, .. } if depth == 0 && !text.starts_with('<') => break,
                ItemBody::Text { text, .. } if depth == 0 && !text.trim().is_empty() => parts.push(text.trim().to_owned()),
                _ => {}
            }
        }
        parts.reverse();
        parts.join("\n\n")
    }

    /// a mermaid diagram (`diagram`) or a formula, drawn once and kept. one
    /// that can't be drawn is shown as its source with the reason, so nothing
    /// the agent wrote is ever hidden.
    #[cfg(feature = "rich")]
    fn picture(&self, diagram: bool, source: String) -> AnyElement {
        let p = self.palette.get();
        let key = (p.dark, diagram, source);
        let mut drawn = self.drawn.borrow_mut();
        if drawn.len() > 300 && !drawn.contains_key(&key) {
            drawn.clear();
        }
        let source = key.2.clone();
        let result = drawn.entry(key).or_insert_with(|| {
            let rgb = |c: Hsla| {
                let c = c.to_rgb();
                [(c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8]
            };
            let style = tessel_rich::Style { dark: p.dark, text: rgb(p.label), muted: rgb(p.secondary), background: rgb(p.window), accent: rgb(p.accent), font_px: 13.0 };
            let svg = if diagram { tessel_rich::mermaid(&source, &style) } else { tessel_rich::math(&source, true, &style) };
            svg.map(|svg| (Arc::new(Image::from_bytes(ImageFormat::Svg, svg.data.into_bytes())), svg.width, svg.height))
        });
        match result {
            Ok((image, w, h)) => {
                // never wider than the column it sits in; never scaled up.
                let scale = ((theme::READING_W - 48.0) / *w).min(1.0);
                h_flex().when(!diagram, |d| d.justify_center()).py_1().child(img(image.clone()).w(px(*w * scale)).h(px(*h * scale))).into_any_element()
            }
            Err(why) => v_flex()
                .gap_1()
                .child(div().p_2().rounded(px(theme::R_TILE)).bg(p.fill).set(Type::Code).text_color(p.label).child(source))
                .child(div().set(Type::Subheadline).text_color(p.secondary).child(format!("Couldn't draw this {}: {why}", if diagram { "diagram" } else { "formula" })))
                .into_any_element(),
        }
    }

    /// the permission requests and questions for the open session. attached to
    /// the composer so it is always in the same place.
    fn prompt_cards(&self, pending: &[PermissionRequest], cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let mut col = v_flex().w_full().pb_2().gap_2();
        for (ix, req) in pending.iter().enumerate() {
            // orchard's card: raised on a hairline, a soft shadow; what it needs from
            // you is said by the blue dot and the words, not by a coloured frame.
            let mut card = v_flex().p(px(14.)).gap(px(10.)).rounded(px(theme::R_CARD)).border_1().border_color(p.separator).bg(p.raised).shadow_sm();
            if req.plan.is_some() {
                // the plan itself is in the transcript, right above.
                let (approve, back) = (req.request_id.clone(), req.request_id.clone());
                card = card
                    .child(div().set(Type::Body).text_color(p.label).child("The plan above is ready for you."))
                    .child(div().set(Type::Subheadline).text_color(p.secondary).child("Approve it to start the work. To change it, type what should be different and choose Keep Planning."))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new(("plan-back", ix)).small().outline().label("Keep Planning").on_click(cx.listener(move |this, _, window, cx| this.keep_planning(back.clone(), window, cx))))
                            .child(Button::new(("plan-approve", ix)).small().primary().label("Approve Plan").on_click(cx.listener(move |this, _, _, _| {
                                this.answer(approve.clone(), PermissionDecision::Allow { updated_input: None })
                            }))),
                    );
            } else if req.is_question {
                let questions = req.input.get("questions").and_then(|q| q.as_array()).cloned().unwrap_or_default();
                let picked = self.answers.get(&req.request_id);
                for (qi, q) in questions.iter().enumerate() {
                    let question = q.get("question").and_then(|v| v.as_str()).unwrap_or("The agent has a question.").to_owned();
                    let chosen = picked.and_then(|m| m.get(&question)).and_then(|v| v.as_str()).map(str::to_owned);
                    let mut opts = h_flex().gap_2().flex_wrap();
                    for (oi, o) in q.get("options").and_then(|v| v.as_array()).into_iter().flatten().enumerate() {
                        let label = o.get("label").and_then(|v| v.as_str()).unwrap_or("?").to_owned();
                        let is_chosen = chosen.as_deref() == Some(label.as_str());
                        let (r, qu, la) = (req.clone(), question.clone(), label.clone());
                        opts = opts.child(
                            Button::new(("option", ix * 10_000 + qi * 100 + oi))
                                .small()
                                .label(label)
                                .when(is_chosen, |b| b.primary())
                                .when(!is_chosen, |b| b.outline())
                                .on_click(cx.listener(move |this, _, _, cx| this.pick(&r, qu.clone(), la.clone(), cx))),
                        );
                    }
                    card = card.child(div().set(Type::Body).text_color(p.label).child(question)).child(opts);
                }
                let rid = req.request_id.clone();
                card = card.child(h_flex().justify_end().child(Button::new(("skip", ix)).small().ghost().label("Don't Answer").on_click(cx.listener(
                    move |this, _, _, _| this.answer(rid.clone(), PermissionDecision::Deny { message: "the user chose not to answer".into() }),
                ))));
            } else {
                let detail = req
                    .input
                    .get("command")
                    .or_else(|| req.input.get("file_path"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| req.input.to_string());
                let (allow, deny) = (req.request_id.clone(), req.request_id.clone());
                let (_, kind, what, _) = tool_parts(&req.tool, &req.input);
                let control = |b: Button| b.h(theme::z(theme::CONTROL_H)).px(theme::z(14.)).rounded(theme::z(theme::R_CONTROL));
                card = card
                    // what it wants, the way a tool row says it.
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .set(Type::Body)
                            .child(h_flex().w(px(14.)).flex_shrink_0().justify_center().child(status_dot(info_blue(&p), false, &p)))
                            .child(div().flex_shrink_0().font_weight(FontWeight::MEDIUM).text_color(p.label).child(kind))
                            .child(div().min_w_0().truncate().text_color(p.secondary).child(what))
                            .child(div().flex_1())
                            .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child("Needs your OK")),
                    )
                    // the whole of it, as it'll run.
                    .child(div().ml(px(22.)).px(px(10.)).py(px(8.)).rounded(px(theme::R_TILE)).bg(p.label.opacity(0.05)).set(Type::Code).text_color(p.secondary).line_clamp(6).child(detail))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .pl(px(22.))
                            .child(h_flex().gap(px(6.)).set(Type::Subheadline).text_color(p.tertiary).child(theme::keycaps(&p, "⌘ ↩")).child("allow"))
                            .child(div().flex_1())
                            .child(control(Button::new(("deny", ix)).outline().label("Deny")).on_click(cx.listener(move |this, _, _, _| {
                                this.answer(deny.clone(), PermissionDecision::Deny { message: String::new() })
                            })))
                            .child(control(Button::new(("allow", ix)).primary().label("Allow")).on_click(cx.listener(move |this, _, _, _| {
                                this.answer(allow.clone(), PermissionDecision::Allow { updated_input: None })
                            }))),
                    );
            }
            col = col.child(card);
        }
        col.into_any_element()
    }

    /// advance everything that is moving. returns true while anything still is.
    fn animate(&mut self, cx: &App) -> bool {
        let cards_moving = self.overview_fade.is_moving() || self.cards.values().any(|c| c.iter().any(Spring::is_moving));
        let drawers_moving = self.drawers.iter().any(|d| d.w.is_moving()) || self.panes_moving();
        let moving = drawers_moving || self.row_fx.is_moving() || self.prompt_open.is_moving() || cards_moving;
        if !moving {
            self.ticker.rest();
            return false;
        }
        // the app's clock, not the wall clock, so a test can step time.
        let dt = self.ticker.dt(cx.background_executor().now());
        let reduced = cx.reduce_motion();
        let mut still = self.step_panes(dt, reduced);
        for d in &mut self.drawers {
            still |= d.w.step(dt, reduced);
        }
        still |= self.row_fx.step(dt, reduced);
        still |= self.prompt_open.step(dt, reduced);
        still |= self.overview_fade.step(dt, reduced);
        for card in self.cards.values_mut() {
            for spring in card.iter_mut() {
                still |= spring.step(dt, reduced);
            }
        }
        if !self.overview && !self.overview_fade.is_moving() && self.overview_fade.value() == 0.0 {
            // fully closed: the cards are sidebar rows again.
            self.cards.clear();
        }
        if !self.prompt_open.is_moving() && self.prompt_open.value() == 0.0 {
            self.prompt_ghost.clear();
        }
        still
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        MARK_PALETTE.with(|c| c.set(self.palette.get()));
        // TESSEL_FRAME_LOG=1: when each frame is built, and how long since the last.
        if std::env::var_os("TESSEL_FRAME_LOG").is_some() {
            thread_local!(static LAST: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) });
            let now = Instant::now();
            let gap = LAST.with(|l| l.replace(Some(now))).map(|t| now.duration_since(t).as_millis());
            FRAME_START.with(|f| f.set(Some(now)));
            eprintln!("frame: {gap:?} ms since the last");
        }
        let active = window.is_window_active();
        if active && !self.window_active {
            // coming back to the window counts as looking at what's open.
            self.window_active = true;
            self.mark_open_seen();
            self.reflow_sidebar();
        }
        self.window_active = active;

        // panes head for wherever the layout and the room now put them.
        self.sync_panes();
        self.sync_surfaces_dock(cx);
        self.sync_terminals(window, cx);
        self.step_bench_term(window, cx);
        self.sync_git_message(window, cx);
        // an `@` wants the session's files.
        if mentions::mention_at_end(&self.composer.read(cx).value()).is_some_and(|(k, ..)| k == mentions::Mention::File) {
            self.want_files();
        }
        if !self.palette_ui.open && std::mem::take(&mut self.theme_preview_by_palette) {
            self.preview_theme(None, window, cx);
        }
        // the panel follows the open session, and a file that has arrived gets its editor.
        self.switch_workspace();
        self.materialize_file(window, cx);

        let animating = self.animate(cx);
        if animating {
            window.request_animation_frame();
        }
        if let Some(interval) = window.refresh_interval(cx) {
            self.refresh_ms = interval.as_secs_f32() * 1000.0;
        }
        if let Some((phase, frames)) = self.bench.as_mut() {
            frames.push((*phase, Instant::now(), animating));
            if !active {
                self.bench_inactive_frames += 1;
            }
        }

        if self.have_sessions
            && let Some(from) = self.bench_from.take()
        {
            // this is when the frame is built, a few ms before it is on glass.
            println!("first frame with sessions: {} ms after main()", from.elapsed().as_millis());
            cx.defer(|cx| cx.quit());
        }

        let viewport = window.viewport_size();
        self.viewport = (f32::from(viewport.width), f32::from(viewport.height));
        if std::mem::take(&mut self.focus_overview) {
            window.focus(&self.overview_focus, cx);
        }
        if window.focused(cx).is_none() {
            window.focus(&self.root_focus, cx);
        }
        if std::mem::take(&mut self.focus_sheet) {
            match self.sheet {
                Some(projects::Sheet::Clone) => self.github_repo.update(cx, |s, cx| s.focus(window, cx)),
                _ => self.project_name.update(cx, |s, cx| s.focus(window, cx)),
            }
        }
        if std::mem::take(&mut self.focus_settings) {
            window.focus(&self.settings_focus, cx);
            // the font fields show what the file says now.
            let (font, mono) = (self.ui.theme.font.clone().unwrap_or_default(), self.ui.theme.mono.clone().unwrap_or_default());
            self.font_input.update(cx, |s, cx| s.set_value(font, window, cx));
            self.mono_input.update(cx, |s, cx| s.set_value(mono, window, cx));
        }

        let now = now_ms();
        let p = self.palette.get();
        let show_overview = self.overview || self.overview_fade.value() > 0.001;
        let ui = self.ui.clone();
        let root = self.node(&ui.root, false, now, cx);
        let font = ui.theme.font.clone();

        // glass: nothing opaque under the frame, so the blurred desktop shows through it.
        // felt: the cloth is drawn under everything, and the frame is clear over it.
        let glass = self.ui.theme.glass.unwrap_or(false);
        let felt = self.felt_on();
        div()
            .id("shell")
            .relative()
            .size_full()
            .when(!glass, |d| d.bg(p.window))
            .when(felt, |d| d.child(self.felt_el(cx)))
            .text_color(p.label)
            .set(Type::Body)
            .when_some(font, |d, f| d.font_family(f))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    this.drag_to(f32::from(ev.position.x), cx);
                }
                if this.pane_drag.is_some() {
                    this.pane_drag_to(f32::from(ev.position.x), f32::from(ev.position.y), cx);
                }
                if this.pane_move.is_some() {
                    this.pane_move_to(f32::from(ev.position.x), f32::from(ev.position.y), cx);
                }
            }))
            .track_focus(&self.root_focus)
            .on_action(cx.listener(|this, _: &crate::SplitRight, window, cx| this.split_focused_right(window, cx)))
            .on_action(cx.listener(|this, _: &crate::SplitDown, window, cx| this.split_focused_down(window, cx)))
            .on_action(cx.listener(|this, _: &crate::NewTerminal, window, cx| {
                if let Err(e) = this.exec("terminal.new", &serde_json::json!({}), window, cx) {
                    this.error = Some(e);
                }
            }))
            .on_action(cx.listener(|this, _: &crate::ClosePane, window, cx| this.close_focused(window, cx)))
            .on_action(cx.listener(|this, _: &crate::Favorite1, window, cx| this.run_key("model.favorite", serde_json::json!({"n": 1}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::Favorite2, window, cx| this.run_key("model.favorite", serde_json::json!({"n": 2}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::Favorite3, window, cx| this.run_key("model.favorite", serde_json::json!({"n": 3}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::Favorite4, window, cx| this.run_key("model.favorite", serde_json::json!({"n": 4}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::Favorite5, window, cx| this.run_key("model.favorite", serde_json::json!({"n": 5}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::NextEffort, window, cx| this.run_key("model.effort", serde_json::json!({}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::ToggleFast, window, cx| this.run_key("model.fast", serde_json::json!({}), window, cx)))
            .on_action(cx.listener(|this, _: &crate::AttachFiles, window, cx| this.run_key("composer.attach", serde_json::json!({}), window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.drag_end(cx);
                this.pane_drag_end(window, cx);
                this.pane_move_end(window, cx);
            }))
            // drop a file anywhere on the window to send it with the next message.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| this.attach_paths(paths.paths().to_vec(), window, cx)))
            .children(root)
            // a broken layout file is said over the top of everything, so no
            // layout can hide its own mistake.
            .when_some(self.ui_error.clone(), |d, e| {
                d.child(
                    h_flex().absolute().top(px(theme::TOOLBAR_H + 8.)).left_0().right_0().justify_center().child(
                        div()
                            .id("ui-error")
                            .test_support()
                            .aria_label(SharedString::from(format!("Layout error: {e}")))
                            .max_w(px(640.))
                            .px_3()
                            .py_2()
                            .rounded(px(theme::R_CARD))
                            .border_1()
                            .border_color(p.failed)
                            .bg(p.window)
                            .shadow_md()
                            .set(Type::Callout)
                            .text_color(p.label)
                            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.failed).child("ui.json didn't load. Showing the last layout that did."))
                            .child(e),
                    ),
                )
            })
            .when(show_overview, |d| d.child(self.overview_layer(now, cx)))
            .when(self.palette_ui.open, |d| d.child(self.palette_layer(window, cx)))
            .when_some(self.sheet, |d, sheet| d.child(self.sheet_layer(sheet, cx)))
            .when(self.settings_open, |d| d.child(self.settings_layer(cx)))
            // TESSEL_FRAME_LOG: painted last, so it sees the whole frame's cost.
            .when(std::env::var_os("TESSEL_FRAME_LOG").is_some(), |d| {
                d.child(
                    canvas(|_, _, _| {}, |_, _, _, _| {
                        if let Some(t) = FRAME_START.with(|f| f.take()) {
                            eprintln!("built, laid out and painted in {} ms", t.elapsed().as_millis());
                        }
                    })
                    .absolute()
                    .size_0(),
                )
            })
    }
}

/// the sidebar's fixed rows above the list.
const NAV_ROW_H: f32 = 30.0;
const NAV_PAD: f32 = 8.0;
/// how far the list starts below the top of the sidebar's content.
pub(crate) const NAV_H: f32 = NAV_ROW_H * 2.0 + NAV_PAD;

/// how far in a session's row starts: under its project's name, past the
/// disclosure and tile.
const SESSION_INDENT: f32 = 28.0;

/// a session's second line, and the time to show beside its name: why it
/// wants you and for how long, or what it is doing and for how long.
fn row_lines(s: &SessionSummary, in_queue: bool, now: Millis) -> (String, Option<String>) {
    if in_queue {
        let a = s.attention.as_ref();
        let why = match a.map(|a| a.reason) {
            Some(AttentionReason::NeedsPermission) => s.activity.as_deref().map(sentence).unwrap_or_else(|| "Needs permission".into()),
            Some(AttentionReason::NeedsInput) => "Asked you a question".into(),
            Some(AttentionReason::Finished) => "Finished".into(),
            Some(AttentionReason::Errored) => "Stopped with an error".into(),
            _ => "Needs you".into(),
        };
        (why, Some(ago(a.map_or(now, |a| a.since_ms), now)))
    } else {
        match (&s.run_state, &s.activity, s.activity_since_ms) {
            (RunState::Working | RunState::Starting | RunState::Stalled, Some(a), Some(since)) => {
                (if s.run_state == RunState::Stalled { format!("Stalled? {a}") } else { sentence(a) }, Some(ago(since, now)))
            }
            (RunState::Idle, None, _) if s.worktree.is_some() => (s.worktree.as_ref().map(|w| w.branch.clone()).unwrap_or_default(), None),
            (state, activity, _) => (activity.as_deref().map(sentence).unwrap_or_else(|| state_word(state).to_owned()), None),
        }
    }
}

/// the trace: how much the agent produced in each of the last sixty seconds'
/// two-second slices, oldest on the left. drawn as one element that paints
/// its bars, so thirty of them cost nothing to lay out.
fn trace(values: &[u16], color: Hsla, width: f32, height: f32) -> impl IntoElement {
    let values = values.to_vec();
    // a quiet agent's few bytes must not be scaled up to look busy.
    let scale = f32::from(values.iter().copied().max().unwrap_or(0)).max(240.0);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let n = values.len().max(1) as f32;
            let slot = bounds.size.width / n;
            for (i, v) in values.iter().enumerate() {
                let frac = (f32::from(*v) / scale).sqrt().clamp(0.0, 1.0);
                // silence is a hairline, so a flat stretch is visibly flat and not just absent.
                let h = (bounds.size.height * frac).max(px(1.));
                let origin = point(bounds.origin.x + slot * i as f32, bounds.origin.y + bounds.size.height - h);
                window.paint_quad(fill(Bounds::new(origin, size((slot - px(1.)).max(px(1.)), h)), color));
            }
        },
    )
    .w(px(width))
    .h(px(height))
    .flex_shrink_0()
}

fn state_word(s: &RunState) -> &'static str {
    match s {
        RunState::Starting => "Starting",
        RunState::Working => "Working",
        RunState::NeedsPermission => "Needs permission",
        RunState::NeedsInput => "Asked a question",
        RunState::Idle => "Idle",
        RunState::Stalled => "Stalled?",
        RunState::Limited { .. } => "Usage limit reached",
        RunState::Errored => "Stopped with an error",
        RunState::Unknown => "",
    }
}

/// what a session's state looks like at a glance.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mark {
    /// blocked on you.
    Diamond(Hsla),
    Check(Hsla),
    Cross(Hsla),
    /// paused for a reason that isn't you (stalled, rate limited): a solid dot.
    Dot(Hsla),
    /// working right now: the tiles chasing round.
    Working(Hsla),
    /// at rest: an empty ring.
    Ring(Hsla),
}

/// each state has its own shape, so colour is never the only thing telling
/// them apart. colour appears only on the three that want you.
fn state_mark(s: &SessionSummary, p: &Palette) -> Mark {
    match (&s.run_state, s.attention.as_ref().map(|a| a.reason)) {
        // orchard's colours: blue waits, amber works, green done, red broken, grey stopped.
        (RunState::NeedsPermission | RunState::NeedsInput, _) => Mark::Diamond(info_blue(p)),
        (RunState::Errored, _) | (_, Some(AttentionReason::Errored)) => Mark::Cross(p.failed),
        (_, Some(AttentionReason::Finished)) => Mark::Check(p.finished),
        (RunState::Working | RunState::Starting, _) => Mark::Working(p.blocked),
        (RunState::Stalled | RunState::Limited { .. }, _) => Mark::Dot(p.blocked.opacity(0.5)),
        _ => Mark::Ring(p.tertiary.opacity(0.7)),
    }
}

/// how often a working dot's ring moves on: about thirty times a second, and
/// only while something is working (see the beat in `Shell::new`).
pub(crate) const BEAT_MS: u64 = 33;

/// one orbit of the working ring, as orchard's StatusDot turns it.
const ORBIT_MS: u64 = 950;

/// queued or waiting on someone else: blue, the one status colour the palette doesn't carry.
pub(crate) fn info_blue(p: &Palette) -> Hsla {
    theme::info(p)
}

/// tessel's status, in orchard's one dot language: an 8pt core that never
/// changes size, its colour the state, and, while work is going on, a ring
/// outside it (so starting or finishing never moves the layout): a faint
/// track and one arc orbiting the core.
/// a moving dot in a slot its ring fits in, for anywhere an edge could clip it.
pub(crate) fn live_dot(core: Hsla, p: &Palette) -> Div {
    h_flex().size(px(18.)).flex_shrink_0().justify_center().child(status_dot(core, true, p))
}

pub(crate) fn status_dot(core: Hsla, live: bool, p: &Palette) -> Div {
    let track = p.label.opacity(0.14);
    let mut dot = div().relative().size(px(8.)).flex_shrink_0().child(div().size_full().rounded_full().bg(core));
    if live {
        let ring = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let c = bounds.center();
                let r = f32::from(bounds.size.width) / 2.0 - 1.2;
                let arc = |from: f32, len: f32| {
                    let mut b = PathBuilder::stroke(px(2.1));
                    let n = 14;
                    for i in 0..=n {
                        let t = from + len * i as f32 / n as f32;
                        let at = point(c.x + px(r * t.cos()), c.y + px(r * t.sin()));
                        if i == 0 { b.move_to(at) } else { b.line_to(at) }
                    }
                    b.build().ok()
                };
                if let Some(path) = arc(0.0, std::f32::consts::TAU) {
                    window.paint_path(path, track);
                }
                let turn = (now_ms() % ORBIT_MS) as f32 / ORBIT_MS as f32;
                if let Some(path) = arc(turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2, std::f32::consts::TAU * 0.22) {
                    window.paint_path(path, core);
                }
            },
        )
        .absolute()
        .left(px(-5.))
        .top(px(-5.))
        .size(px(18.));
        dot = dot.child(ring);
    }
    dot
}

thread_local! {
    /// when the frame being drawn started building, for TESSEL_FRAME_LOG.
    static FRAME_START: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    /// the palette marks are drawn in, set as each frame starts.
    static MARK_PALETTE: std::cell::Cell<Palette> = std::cell::Cell::new(Palette::dark());
}

/// draw a mark in a fixed 14pt box so rows line up whatever the shape.
fn mark(m: Mark) -> impl IntoElement {
    let slot = h_flex().size(px(14.)).flex_shrink_0().justify_center();
    // every state is orchard's dot: the colour says which, the ring says it's moving,
    // and the words beside it always say it too.
    let p = MARK_PALETTE.with(|c| c.get());
    match m {
        Mark::Working(c) => slot.items_center().child(status_dot(c, true, &p)),
        Mark::Dot(c) | Mark::Ring(c) | Mark::Diamond(c) | Mark::Check(c) | Mark::Cross(c) => slot.items_center().child(status_dot(c, false, &p)),
    }
}

/// the report inside a subagent's hand-back, which arrives as a user message
/// wrapped in `<agent-message>` with a preamble for the model. `None` for
/// anything a person actually wrote.
fn handback(text: &str) -> Option<String> {
    let body = text.trim_start().strip_prefix("<agent-message")?;
    let body = body.split_once('>')?.1;
    let body = body.rsplit_once("</agent-message>").map_or(body, |(b, _)| b);
    let report = body.split_once("The report follows:").map_or(body, |(_, r)| r);
    // the harness indents every line of the report by two spaces.
    let lines: Vec<&str> = report.lines().map(|l| l.strip_prefix("  ").unwrap_or(l)).collect();
    Some(lines.join("\n").trim().to_owned())
}

impl Shell {
    /// what you said, as marks down the transcript's edge: hover for the list,
    /// click to go there.
    pub(crate) fn message_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let Some(open) = self.model.open.as_ref() else { return div().into_any_element() };
        let mut said: Vec<(usize, String)> = Vec::new();
        for ix in 0..open.len() {
            if let Some((item, 0)) = open.row(ix)
                && let ItemBody::User { text, .. } = &item.body
                && handback(text).is_none()
                && !text.trim().is_empty()
            {
                said.push((ix, text.lines().next().unwrap_or_default().chars().take(90).collect()));
            }
        }
        if said.len() < 2 {
            return div().into_any_element();
        }
        // at most two dozen marks; a long session shows every few.
        let step = said.len().div_ceil(24);
        let marks: Vec<&(usize, String)> = said.iter().step_by(step).collect();
        let mut ticks = v_flex().w(px(20.)).h_full().justify_center().items_center().gap(px(6.));
        for (i, _) in marks.iter().enumerate() {
            let last = i + 1 == marks.len();
            ticks = ticks.child(div().w(px(10.)).h(px(2.)).rounded_full().bg(if last { p.tertiary } else { p.quaternary }));
        }
        let mut rail = h_flex()
            .id("message-rail")
            .test_support()
            .aria_label("Your messages")
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(4.))
            .items_center()
            .justify_end()
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                this.rail_open = *over;
                cx.notify();
            }));
        if self.rail_open {
            let mut list = theme::float_list(&p).id("message-rail-list").max_h(px(420.)).overflow_y_scroll();
            for (ix, text) in &said {
                let ix = *ix;
                list = list.child(theme::float_item(&p, SharedString::from(format!("rail-{ix}")), None, text.clone(), None, false).on_click(cx.listener(move |this, _, _, cx| {
                    this.rail_open = false;
                    this.scroller.update(cx, |s, cx| {
                        s.scroll_to_item(ix, cx);
                    });
                    cx.notify();
                })));
            }
            rail = rail.child(div().w(px(300.)).mr_1().rounded(px(theme::R_PANEL)).border_1().border_color(p.separator).bg(p.raised).shadow_lg().child(list));
        }
        rail.child(ticks).into_any_element()
    }
}

/// "you clicked Stop", from `<surface-event ... element="stop" act="click"/>`.
fn surface_event_line(text: &str) -> String {
    let attr = |name: &str| text.split(&format!("{name}=\"")).nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_owned();
    let (element, act, value) = (attr("element"), attr("act"), attr("value"));
    match act.as_str() {
        "click" => format!("You clicked {element}"),
        _ if !value.is_empty() => format!("You set {element} to {value}"),
        _ => format!("You changed {element}"),
    }
}

#[cfg(test)]
pub(crate) fn handback_for_test(text: &str) -> Option<String> {
    handback(text)
}

/// first letter capitalised. activity lines from the daemon are lowercase fragments.
fn sentence(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// files the open session wrote or edited, most recently touched first, with how many times.
fn tool_row(tool: &str, input: &serde_json::Value, status: ToolStatus, output: Option<&str>, ran_ms: Option<Millis>, p: &Palette) -> impl IntoElement {
    let (icon, kind, detail, mono) = tool_parts(tool, input);
    // the tool's own icon at rest; while it isn't done, what state it's in.
    let lead = match status {
        ToolStatus::Ok => div()
            .size(px(15.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(15.)).text_color(p.secondary))
            .into_any_element(),
        ToolStatus::Pending | ToolStatus::Running => mark(Mark::Working(p.blocked)).into_any_element(),
        ToolStatus::AwaitingPermission => mark(Mark::Diamond(info_blue(p))).into_any_element(),
        ToolStatus::Failed => mark(Mark::Cross(p.failed)).into_any_element(),
        ToolStatus::Denied | ToolStatus::Aborted | ToolStatus::Unknown => mark(Mark::Ring(p.tertiary)).into_any_element(),
    };
    let note = match status {
        ToolStatus::AwaitingPermission => Some("waiting for you"),
        ToolStatus::Denied => Some("denied"),
        ToolStatus::Aborted => Some("cut off"),
        ToolStatus::Failed => Some("failed"),
        _ => None,
    };
    let took = ran_ms.filter(|ms| *ms >= 1000).map(|ms| ago(0, ms));
    let mut col = v_flex().gap(px(6.)).child(
        h_flex()
            .gap(px(8.))
            .min_h(px(24.))
            .set(Type::Callout)
            .child(lead)
            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).text_color(p.label).child(kind))
            .child(div().flex_1().min_w_0().truncate().text_color(p.secondary).when(mono, |d| d.set(Type::Code)).child(detail))
            .children(note.map(|n| div().flex_shrink_0().text_color(match status {
                ToolStatus::Failed => p.failed,
                ToolStatus::AwaitingPermission => info_blue(p),
                _ => p.tertiary,
            }).child(n)))
            .children(took.map(|t| div().flex_shrink_0().set(Type::Code).text_color(p.tertiary).child(t))),
    );
    // output earns its space only when something went wrong.
    if status == ToolStatus::Failed
        && let Some(out) = output.filter(|o| !o.trim().is_empty())
    {
        col = col.child(div().ml(px(7.)).pl(px(16.)).border_l_1().border_color(p.separator).set(Type::Code).text_color(p.secondary).line_clamp(8).child(out.to_owned()));
    }
    col
}

/// a tool call as an icon, a word for what kind of thing it did, and what it
/// did it to; and whether that last part is code (a command, a pattern).
fn tool_parts(tool: &str, input: &serde_json::Value) -> (&'static str, String, String, bool) {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty());
    let base = |p: &str| p.rsplit('/').find(|x| !x.is_empty()).unwrap_or(p).to_owned();
    let line = |t: &str| t.lines().next().unwrap_or("").trim().to_owned();
    match tool {
        "Read" => ("file-text", "Read".into(), s("file_path").map(base).unwrap_or_default(), false),
        "Write" => ("file-plus", "Write".into(), s("file_path").map(base).unwrap_or_default(), false),
        "Edit" | "MultiEdit" => ("pencil", "Edit".into(), s("file_path").map(base).unwrap_or_default(), false),
        "NotebookEdit" => ("pencil", "Edit".into(), s("notebook_path").map(base).unwrap_or_default(), false),
        "Bash" | "BashOutput" => match (s("description"), s("command")) {
            (Some(d), _) => ("square-terminal", "Run".into(), line(d), false),
            (None, Some(c)) => ("square-terminal", "Run".into(), line(c), true),
            _ => ("square-terminal", "Run".into(), String::new(), false),
        },
        "Grep" | "Glob" => ("search", "Search".into(), s("pattern").map(line).unwrap_or_default(), true),
        "WebFetch" => ("globe", "Fetch".into(), s("url").map(line).unwrap_or_default(), false),
        "WebSearch" => ("globe", "Search the web".into(), s("query").map(line).unwrap_or_default(), false),
        "Task" | "Agent" => ("users", "Task".into(), s("description").map(line).unwrap_or_default(), false),
        "TodoWrite" => ("list-checks", "To-dos".into(), String::new(), false),
        "ToolSearch" => ("wrench", "Tool".into(), "loading one".into(), false),
        t if t.starts_with("mcp__") => {
            let mut parts = t.trim_start_matches("mcp__").splitn(2, "__");
            let server = parts.next().unwrap_or_default().to_owned();
            let name = parts.next().unwrap_or_default().replace('_', " ");
            ("plug", server, name, false)
        }
        t => ("wrench", t.to_owned(), summarize_tool(t, input), false),
    }
}

/// a system notification, for when tessel isn't the app in front.
fn notify_os(goal: &str, reason: AttentionReason) {
    let what = match reason {
        AttentionReason::NeedsPermission => "Needs your permission",
        AttentionReason::NeedsInput => "Asked you a question",
        AttentionReason::Finished => "Finished",
        AttentionReason::Errored => "Stopped with an error",
        AttentionReason::Unknown => return,
    };
    if std::env::var_os("TESSEL_NO_NOTIFY").is_some() || cfg!(test) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        let script = format!("display notification {} with title {}", quote(what), quote(goal));
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (goal, what);
    }
}

/// the card a turn's time opens: what answered, from when to when, and what it took.
#[derive(Clone)]
struct TurnTip {
    p: Palette,
    title: String,
    when: String,
    rows: Vec<(String, String)>,
}

impl Render for TurnTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = self.p;
        let mut card = v_flex()
            .w(px(300.))
            .p(px(12.))
            .gap(px(4.))
            .rounded(px(theme::R_FLOAT))
            .border_1()
            .border_color(p.separator)
            .bg(p.raised)
            .shadow_lg()
            .set(Type::Callout)
            .child(div().font_weight(FontWeight::MEDIUM).text_color(p.label).child(self.title.clone()))
            .child(div().pb(px(6.)).text_color(p.secondary).child(self.when.clone()))
            .child(div().h(px(1.)).bg(p.separator).mb(px(4.)));
        for (k, v) in &self.rows {
            card = card.child(h_flex().child(div().flex_1().text_color(p.secondary).child(k.clone())).child(div().set(Type::Code).text_color(p.label).child(v.clone())));
        }
        card
    }
}
