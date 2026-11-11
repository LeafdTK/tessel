//! photon: the app. a thin window over the daemon.
//!
//! a library with a small launcher (`main.rs`) so that the ui tests and the
//! benchmark harness can reach the same code the real app runs.

pub mod bridge;
pub mod commands;
pub mod graph;
pub mod model;
pub mod motion;
pub mod rich;
pub mod shell;
#[cfg(test)]
mod shell_tests;
pub mod theme;
pub mod ui;

use gpui_kit::*;
use shell::{Launch, Shell};

actions!(
    photon,
    [
        NewSession,
        ToggleSidebar,
        ToggleInspector,
        ToggleFocusMode,
        ToggleOverview,
        NextAttention,
        StopAgent,
        AllowPending,
        OpenSettings,
        CloseSettings,
        OpenAsk,
        CloseAsk,
        CloseSheet,
        ConfirmSheet,
        OpenPalette,
        PaletteClose,
        PaletteUp,
        PaletteDown,
        PaletteScope,
        SplitRight,
        SplitDown,
        NewTerminal,
        ClosePane,
        MaximizePane,
        Quit,
        // only while the overview is up:
        CloseOverview,
        OverviewOpen,
        OverviewLeft,
        OverviewRight,
        OverviewUp,
        OverviewDown
    ]
);

/// the keyboard map. these are the mac conventions where one exists:
/// ⌘. is cancel, ⌥⌘S is the sidebar, ⌥⌘I is the inspector.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewSession, None),
        KeyBinding::new("alt-cmd-s", ToggleSidebar, None),
        KeyBinding::new("alt-cmd-i", ToggleInspector, None),
        KeyBinding::new("shift-cmd-f", ToggleFocusMode, None),
        // the same key safari uses for its tab overview.
        KeyBinding::new("shift-cmd-\\", ToggleOverview, None),
        KeyBinding::new("escape", CloseOverview, Some("Overview")),
        KeyBinding::new("enter", OverviewOpen, Some("Overview")),
        KeyBinding::new("left", OverviewLeft, Some("Overview")),
        KeyBinding::new("right", OverviewRight, Some("Overview")),
        KeyBinding::new("up", OverviewUp, Some("Overview")),
        KeyBinding::new("down", OverviewDown, Some("Overview")),
        KeyBinding::new("cmd-j", NextAttention, None),
        KeyBinding::new("cmd-.", StopAgent, None),
        KeyBinding::new("cmd-enter", AllowPending, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-k", OpenPalette, None),
        KeyBinding::new("shift-cmd-k", OpenAsk, None),
        KeyBinding::new("escape", PaletteClose, Some("Palette")),
        KeyBinding::new("up", PaletteUp, Some("Palette")),
        KeyBinding::new("down", PaletteDown, Some("Palette")),
        KeyBinding::new("ctrl-p", PaletteUp, Some("Palette")),
        KeyBinding::new("ctrl-n", PaletteDown, Some("Palette")),
        KeyBinding::new("tab", PaletteScope, Some("Palette")),
        // the pane last clicked in.
        KeyBinding::new("cmd-d", SplitRight, None),
        KeyBinding::new("shift-cmd-d", SplitDown, None),
        KeyBinding::new("ctrl-`", NewTerminal, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("shift-cmd-enter", MaximizePane, None),
        KeyBinding::new("escape", CloseAsk, Some("Ask")),
        KeyBinding::new("escape", CloseSheet, Some("Sheet")),
        KeyBinding::new("cmd-enter", ConfirmSheet, Some("Sheet")),
        KeyBinding::new("escape", CloseSettings, Some("Settings")),
    ]);
}

/// every command is in the menu bar, as the mac expects. shortcuts shown in
/// the menus come from the key bindings, so this must run after `bind_keys`.
fn set_menus(cx: &mut App) {
    let menu = |name: &'static str, items: Vec<MenuItem>| Menu { name: name.into(), items, disabled: false };
    cx.set_menus(vec![
        menu("photon", vec![MenuItem::action("Settings…", OpenSettings), MenuItem::separator(), MenuItem::action("Quit photon", Quit)]),
        menu("File", vec![MenuItem::action("New Session", NewSession)]),
        menu(
            "View",
            vec![
                MenuItem::action("Show or Hide Sidebar", ToggleSidebar),
                MenuItem::action("Show or Hide Inspector", ToggleInspector),
                MenuItem::action("All Sessions", ToggleOverview),
                MenuItem::separator(),
                MenuItem::action("Focus on This Session", ToggleFocusMode),
                MenuItem::separator(),
                MenuItem::action("Split Pane Right", SplitRight),
                MenuItem::action("Split Pane Down", SplitDown),
                MenuItem::action("New Terminal", NewTerminal),
                MenuItem::action("Maximize Pane", MaximizePane),
                MenuItem::action("Close Pane", ClosePane),
                MenuItem::separator(),
                MenuItem::action("Command Palette…", OpenPalette),
                MenuItem::action("Change This Window…", OpenAsk),
            ],
        ),
        menu(
            "Session",
            vec![
                MenuItem::action("Next Session That Needs You", NextAttention),
                MenuItem::separator(),
                MenuItem::action("Allow", AllowPending),
                MenuItem::action("Stop the Agent", StopAgent),
            ],
        ),
    ]);
}

/// route the app-level commands to the window. app-level so they work
/// whatever has keyboard focus, and so their menu items are enabled.
pub fn bind_actions(shell: WeakEntity<Shell>, cx: &mut App) {
    fn on<A: Action>(cx: &mut App, shell: &WeakEntity<Shell>, f: fn(&mut Shell, &mut Context<Shell>)) {
        let shell = shell.clone();
        cx.on_action(move |_: &A, cx| {
            let _ = shell.update(cx, f);
        });
    }
    on::<NewSession>(cx, &shell, Shell::show_new_session);
    on::<ToggleSidebar>(cx, &shell, Shell::toggle_sidebar);
    on::<ToggleInspector>(cx, &shell, Shell::toggle_inspector);
    on::<ToggleFocusMode>(cx, &shell, Shell::toggle_focus_mode);
    on::<ToggleOverview>(cx, &shell, Shell::toggle_overview);
    on::<NextAttention>(cx, &shell, Shell::next_attention);
    on::<StopAgent>(cx, &shell, Shell::stop_agent);
    on::<AllowPending>(cx, &shell, Shell::allow_pending);
    on::<OpenSettings>(cx, &shell, Shell::open_settings);
    on::<OpenAsk>(cx, &shell, Shell::open_ask);
    on::<OpenPalette>(cx, &shell, Shell::open_palette);
    on::<MaximizePane>(cx, &shell, Shell::zoom_focused);
    // splitting and closing panes change ui.json, which needs the window: the
    // window's root handles those (see `Shell::render`).
}

/// the window: a toolbar merged with the title bar, with the close/minimise/zoom
/// buttons inset to sit on its centre line.
pub fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(theme::CONTENT_MIN + 200.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("photon".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(19.), px(19.))),
        }),
        // we draw the title bar, so we move the window; otherwise macos also
        // handles the drag and delays every click in the toolbar.
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// put the layout reference and the shipped presets where an agent (or a
/// person) editing `ui.json` will find them. rewritten every launch, so they
/// always describe the build that is running. best effort: a read-only home
/// costs the docs, not the window.
pub fn write_reference(home: &std::path::Path) {
    let dir = home.join("ui");
    let _ = std::fs::create_dir_all(dir.join("presets"));
    let _ = std::fs::write(dir.join("README.md"), ui::REFERENCE);
    for (name, text) in ui::PRESETS {
        let _ = std::fs::write(dir.join("presets").join(format!("{name}.json")), text);
    }
}

/// open the window and run until quit.
pub fn run(mut launch: Launch) {
    let home = photon_proto::local::home();
    write_reference(&home);
    launch.ui.get_or_insert_with(|| home.join("ui.json"));
    // the whole lucide catalog, not the kit's default handful: a layout may name any icon.
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        gpui_kit::init(cx);
        bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let (bridge, events) = bridge::start(home.clone());
        let options = window_options(cx);
        let (_, shell) = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Shell::new(bridge, events, launch, window, cx)))
            .expect("failed to open the window");
        bind_actions(shell.downgrade(), cx);
        set_menus(cx);
        cx.activate(true);
    });
}
