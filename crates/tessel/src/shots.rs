use crate::bridge::{Bridge, UiEvent};
use crate::shell::{Launch, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, VisualTestAppContext, WindowHandle, px, size};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tessel_proto::proof::{Finding, Proof, Reviewer, Severity, Tried, Verdict, Verified};
use tessel_proto::task::{Risk, Task, TaskNote, TaskState};
use tessel_proto::views::{Graded, Metrics};
use tessel_proto::*;
use tokio::sync::mpsc;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

fn chat(n: u128, goal: &str, state: RunState, cost: f64, proof: Option<Proof>) -> SessionSummary {
    SessionSummary {
        id: sid(n),
        goal: goal.into(),
        cwd: format!("/Users/you/code/shop-{n}"),
        worktree: Some(WorktreeInfo { repo: "/Users/you/code/shop".into(), branch: format!("tessel/t{n}") }),
        parent: None,
        activity: (state == RunState::Working).then(|| "running cargo test".into()),
        activity_since_ms: Some(now() - 40_000),
        attention: (state == RunState::Idle).then(|| Attention { reason: AttentionReason::Finished, since_ms: now() - 60_000, seq: 1 }),
        run_state: state,
        warm: true,
        background_tasks: 0,
        model: Some("sonnet".into()),
        permission_mode: None,
        effort: None,
        fast: false,
        context_used: Some(42_000),
        context_window: Some(200_000),
        tasks: vec![],
        mcp: vec![],
        linked_dirs: vec![],
        visual: false,
        cost_usd: cost,
        trace: vec![],
        asking: None,
        run: None,
        proof,
        last_seq: 6,
        created_ms: now() - n as u64 * 600_000,
        updated_ms: now() - n as u64 * 60_000,
    }
}

fn proof(shot: Option<&Path>) -> Proof {
    let finding = |lens: &str, file: &str, line: Option<u32>, severity: Severity, rule: Option<&str>, title: &str, why: &str| Finding {
        lens: lens.into(),
        rule: rule.map(str::to_owned),
        file: Some(file.into()),
        line,
        severity,
        title: title.into(),
        why: why.into(),
        confirmed: Some(true),
    };
    Proof {
        started_ms: now() - 150_000,
        finished_ms: Some(now() - 12_000),
        head: "4f2c9e1".into(),
        verdict: Verdict::Concerns,
        verify: vec![
            Verified { command: "cargo test -q".into(), ok: true, output: String::new(), ms: 41_000, rule: None },
            Verified { command: "cargo clippy -q -- -D warnings".into(), ok: true, output: String::new(), ms: 22_000, rule: None },
        ],
        tried: vec![
            Tried { id: "checkout".into(), title: "Checking out with a coupon".into(), ok: true, said: String::new(), ms: 61_000, cost_usd: 0.03, holdout: false, broken: false },
            Tried { id: "refund".into(), title: "Refunding half an order".into(), ok: true, said: String::new(), ms: 48_000, cost_usd: 0.02, holdout: true, broken: false },
        ],
        findings: vec![
            finding("look", "cart.png", None, Severity::Warn, Some("roomy-controls"), "The Apply button touches the coupon field", "Bottom right of the cart: no gap between the field and the button."),
            finding("correctness", "src/coupon.rs", Some(88), Severity::Warn, None, "An expired coupon still applies until midnight UTC", "expires_at is compared to the local date, not the instant."),
            finding("scope", "src/cart/view.rs", Some(12), Severity::Note, None, "Renames a helper the change doesn't need", "cart_total became total_for."),
        ],
        reviewers: vec![
            Reviewer { lens: "look".into(), error: None, cost_usd: 0.02, ms: 19_000 },
            Reviewer { lens: "correctness · scope".into(), error: None, cost_usd: 0.01, ms: 33_000 },
        ],
        rules: vec!["roomy-controls".into(), "commit-style".into()],
        files: 4,
        cost_usd: 0.08,
        shots: shot.map(|p| vec![p.to_string_lossy().into_owned()]).unwrap_or_default(),
        ..Default::default()
    }
}

fn task(id: &str, title: &str, state: TaskState, session: Option<u128>, note: Option<&str>, risk: Option<Risk>, landed: Option<&str>, pr: Option<&str>) -> Task {
    Task {
        id: id.into(),
        title: title.into(),
        spec: String::new(),
        repo: "/Users/you/code/shop".into(),
        state,
        deps: vec![],
        parent: None,
        session: session.map(sid),
        notes: note.map(|n| vec![TaskNote { at_ms: now() - 30_000, by: "tessel".into(), text: n.into() }]).unwrap_or_default(),
        plan: false,
        attempts: 0,
        proof_cost_usd: 0.03,
        budget_usd: None,
        risk,
        landed: landed.map(str::to_owned),
        pr: pr.map(str::to_owned),
        issue: (id == "t7a8b").then(|| "https://github.com/you/shop/issues/57".to_owned()),
        attachments: vec![],
        land: None,
        ask: (state == TaskState::Review).then(|| "It changes how coupons stack with sales, which the task didn't say. Should a coupon apply on top of a sale price?".to_owned()),
        summary: Some("Added a coupon field to the cart. Codes are checked on the server; percent and fixed discounts apply before tax, and expired codes are refused with a message under the field.".to_owned()),
        changes: vec![tessel_proto::task::FileStat { path: "src/cart/coupon.rs".into(), added: 84, removed: 3 }, tessel_proto::task::FileStat { path: "src/cart/view.rs".into(), added: 22, removed: 9 }, tessel_proto::task::FileStat { path: "tests/coupon.rs".into(), added: 61, removed: 0 }],
        review: (state == TaskState::Landed).then(|| "Does what was asked, and the tests cover both kinds of discount.".to_owned()),
        cleared: false,
        created_ms: now() - 3_600_000,
        updated_ms: now() - 60_000,
    }
}

fn tasks() -> Vec<Task> {
    vec![
        task("t1a2b", "Coupons at checkout", TaskState::Review, Some(1), Some("proven with concerns"), Some(Risk::Medium), None, None),
        task("t3c4d", "Show stock left on the product page", TaskState::Running, Some(2), None, None, None, None),
        task("t5e6f", "Retry failed webhooks with backoff", TaskState::Checking, Some(3), Some("Opened a pull request into main. Waiting for its checks."), Some(Risk::Low), None, Some("https://github.com/you/shop/pull/42")),
        task("t7a8b", "Fix the cart badge count after removing an item", TaskState::Ready, None, None, None, None, None),
        task("t9c0d", "Fix how it looks: the search field clips its placeholder", TaskState::Draft, None, None, None, None, None),
        task("tab12", "Add a sitemap", TaskState::Landed, Some(4), Some("Merged into main."), Some(Risk::Low), Some("8d1f0a2"), None),
    ]
}

fn metrics() -> Metrics {
    let g = |lens: &str, found: u32, refuted: u32, fixed: u32, ignored: u32| Graded { lens: lens.into(), found, refuted, fixed, ignored };
    Metrics {
        landed: 14,
        review: 1,
        working: 2,
        ready: 1,
        dropped: 1,
        cost_per_landed: 0.41,
        waste: 0.22,
        escaped: 0,
        lenses: vec![g("correctness", 23, 4, 15, 2), g("scope", 17, 6, 8, 3), g("look", 9, 1, 7, 1), g("edge-cases", 8, 2, 5, 0), g("security", 3, 1, 2, 0)],
        spend: (0..6u64).map(|d| (now() - d * 86_400_000 - 3_600_000, [1.49, 2.1, 0.8, 3.4, 0.0, 1.2][d as usize])).collect(),
    }
}

fn sessions(shot: Option<&Path>) -> Vec<SessionSummary> {
    vec![
        chat(1, "Coupons at checkout", RunState::Idle, 0.62, Some(proof(shot))),
        chat(2, "Show stock left on the product page", RunState::Working, 0.31, None),
        chat(3, "Retry failed webhooks with backoff", RunState::Idle, 0.44, None),
        chat(4, "Add a sitemap", RunState::Idle, 0.12, None),
    ]
}

fn snapshot(shot: Option<&Path>) -> UiEvent {
    let item = |n: u64, body: ItemBody| Item { id: ItemId::from(format!("i{n}").as_str()), parent: None, seq: n, order: n, created_ms: now() - 200_000 + n * 1000, body };
    UiEvent::Snapshot(Box::new(SessionSnapshot {
        summary: sessions(shot).remove(0),
        items: vec![
            item(1, ItemBody::User { text: "Coupons at checkout: a code field in the cart, percent or fixed amount, with an expiry.".into(), attachments: vec![] }),
            item(2, ItemBody::Text { text: "Added a coupon field to the cart. Codes are checked on the server; percent and fixed discounts both apply before tax, and expired codes are refused with a message under the field. Tests cover both kinds and expiry.".into(), streaming: false }),
        ],
        has_more: false,
        pending: vec![],
    }))
}

struct Screen {
    name: &'static str,
    launch: fn() -> Launch,
    calls: &'static [(&'static str, &'static str)],
}

const SCREENS: &[Screen] = &[
    Screen { name: "new", launch: Launch::default, calls: &[] },
    Screen {
        name: "chat-checks",
        launch: || Launch { open: Some("00000000-0000-0000-0000-000000000001".into()), panel: Some("checks".into()), ..Default::default() },
        calls: &[("app.factory", r#"{"on": true}"#)],
    },
    Screen { name: "board", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "board", "pane": "@focused"}"#)] },
    Screen { name: "factory", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "views"}"#)] },
    Screen { name: "home", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "home", "pane": "@focused"}"#)] },
    Screen { name: "board-open", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "board", "pane": "@focused"}"#)] },
    Screen { name: "board-paused", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "board", "pane": "@focused"}"#)] },
    Screen { name: "settings-factory", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("app.settings", r#"{"section": "factory"}"#)] },
    Screen { name: "board-empty", launch: Launch::default, calls: &[("app.factory", r#"{"on": true}"#), ("pane.show", r#"{"module": "board", "pane": "@focused"}"#)] },
];

fn settle(cx: &mut VisualTestAppContext, window: &WindowHandle<Shell>) {
    for _ in 0..30 {
        cx.run_until_parked();
        cx.advance_clock(Duration::from_millis(50));
        let _ = cx.update_window((*window).into(), |_, window, cx| window.render_frame(cx));
    }
    cx.run_until_parked();
}

fn save(frame: image::RgbaImage, path: &Path) -> Result<(), String> {
    frame.save(path).map_err(|e| format!("saving {}: {e}", path.display()))
}

pub fn run(dir: &Path, dark: bool) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let platform = gpui_kit::platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(gpui_kit::assets::AllAssets));
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::add_fonts(cx);
        crate::bind_keys(cx);
    });
    let mut saved = Vec::new();
    for screen in SCREENS {
        let (cmd_tx, mut cmds) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        let scratch = std::env::temp_dir().join(format!("tessel-shots-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&scratch);
        let launch = Launch { dark: Some(dark), ui: Some(scratch.join("ui.json")), ..(screen.launch)() };
        let window = cx
            .open_offscreen_window(size(px(1440.), px(900.)), move |window, cx| cx.new(|cx| Shell::new(Bridge { cmds: cmd_tx }, event_rx, launch, window, cx)))
            .map_err(|e| format!("opening a window: {e:#}"))?;
        let _ = window.update(&mut cx, |s, _, _| s.hosts = crate::shell::hosts::HostsView::new(scratch.clone()));
        let shot = saved.first().map(PathBuf::as_path);
        let settings = DaemonSettings { paused: screen.name.ends_with("-paused"), ..DaemonSettings::default() };
        for ev in [UiEvent::Connected, UiEvent::On { host: None, via: "this Mac".into() }, UiEvent::DaemonSettings(settings), UiEvent::Sessions(sessions(shot)), snapshot(shot)] {
            let _ = events.send(ev);
        }
        settle(&mut cx, &window);
        for (cmd, args) in screen.calls {
            let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
            window.update(&mut cx, |s, window, cx| s.exec(cmd, &args, window, cx)).map_err(|e| format!("{e:#}"))?.map_err(|e| format!("{} on {}: {e}", cmd, screen.name))?;
            settle(&mut cx, &window);
        }
        for ev in [
            UiEvent::Answer { tag: "tasks", result: Ok(Response::Tasks { tasks: if screen.name.ends_with("-empty") { Vec::new() } else { tasks() } }) },
            UiEvent::Answer { tag: "metrics", result: Ok(Response::Metrics { metrics: metrics() }) },
            UiEvent::Usage(vec![UsageWindow { name: "five_hour".into(), used: 0.42, resets_at: Some((now() / 1000) as i64 + 3 * 3600 + 600), status: "allowed".into() }]),
        ] {
            let _ = events.send(ev);
        }
        if screen.name == "board-open" {
            let _ = window.update(&mut cx, |s, _, _| {
                s.open_task = Some("t1a2b".into());
                s.show_working = true;
            });
        }
        settle(&mut cx, &window);
        while cmds.try_recv().is_ok() {}
        let frame = cx.capture_screenshot(window.into()).map_err(|e| format!("picturing {}: {e:#}", screen.name))?;
        let path = dir.join(format!("{}.png", screen.name));
        save(frame, &path)?;
        saved.push(path);
        let _ = window.update(&mut cx, |_, window, _| window.remove_window());
        cx.run_until_parked();
        let _ = std::fs::remove_dir_all(&scratch);
    }
    Ok(saved)
}
