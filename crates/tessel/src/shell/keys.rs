use super::*;
use gpui_kit::component::Icon;
use tessel_proto::keys::KeyInfo;
use tessel_proto::{Request, Response};

pub(crate) struct KeyForm {
    pub name: Entity<InputState>,
    pub host: Entity<InputState>,
    pub env: Entity<InputState>,
    pub secret: Entity<InputState>,
}

impl KeyForm {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            name: cx.new(|cx| InputState::new(window, cx).placeholder("name, like stripe")),
            host: cx.new(|cx| InputState::new(window, cx).placeholder("api.stripe.com")),
            env: cx.new(|cx| InputState::new(window, cx).placeholder("STRIPE_KEY (optional)")),
            secret: cx.new(|cx| InputState::new(window, cx).placeholder("the secret").masked(true)),
        }
    }
}

impl Shell {
    pub(crate) fn keys_answer(&mut self, result: Result<Response, String>) {
        match result {
            Ok(Response::Keys { keys, uses, .. }) => self.keys = Some((keys, uses)),
            Ok(_) => {}
            Err(e) => self.error = Some(e),
        }
    }

    fn add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let f = &self.key_inputs;
        let read = |e: &Entity<InputState>, cx: &Context<Self>| e.read(cx).value().trim().to_owned();
        let (name, host, env, secret) = (read(&f.name, cx), read(&f.host, cx), read(&f.env, cx), read(&f.secret, cx));
        if name.is_empty() || host.is_empty() || secret.is_empty() {
            self.error = Some("A key needs a name, the host it's for, and its secret.".into());
            cx.notify();
            return;
        }
        let key = KeyInfo { name, hosts: host.split(',').map(|h| h.trim().to_owned()).filter(|h| !h.is_empty()).collect(), header: "authorization".into(), format: "Bearer {key}".into(), env: (!env.is_empty()).then_some(env), ..Default::default() };
        self.cmd(Cmd::Ask { tag: "keys", req: Request::KeySet { key, value: Some(secret) } });
        for e in [f.name.clone(), f.host.clone(), f.env.clone(), f.secret.clone()] {
            e.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.notify();
    }

    fn github_from_cli(&mut self, cx: &mut Context<Self>) {
        let token = std::process::Command::new("gh").args(["auth", "token"]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|t| !t.is_empty());
        let Some(token) = token else {
            self.error = Some("The GitHub CLI isn't signed in here: run gh auth login, then try again.".into());
            cx.notify();
            return;
        };
        let api = KeyInfo { name: "github".into(), hosts: vec!["api.github.com".into(), "uploads.github.com".into()], header: "authorization".into(), format: "Bearer {key}".into(), env: Some("GH_TOKEN".into()), ..Default::default() };
        let git = KeyInfo { name: "github-git".into(), hosts: vec!["github.com".into()], header: "authorization".into(), format: "Basic {basic:x-access-token}".into(), ..Default::default() };
        self.cmd(Cmd::Ask { tag: "keys", req: Request::KeySet { key: api, value: Some(token.clone()) } });
        self.cmd(Cmd::Ask { tag: "keys", req: Request::KeySet { key: git, value: Some(token) } });
        cx.notify();
    }

    pub(crate) fn keys_settings(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let dim = |t: String| div().set(Type::Subheadline).text_color(p.secondary).child(t);
        let mut col = v_flex().gap_2();
        let Some((keys, uses)) = &self.keys else { return col.child(dim("Asking the daemon…".into())) };
        for k in keys {
            let name = k.name.clone();
            let detail = format!(
                "{}{}{}",
                k.hosts.join(", "),
                k.env.as_deref().map(|e| format!(" · ${e}")).unwrap_or_default(),
                if k.stored { String::new() } else { " · no secret on this machine".into() }
            );
            col = col.child(
                h_flex()
                    .id(ElementId::from(SharedString::from(format!("keyrow-{name}"))))
                    .test_support()
                    .gap_2()
                    .py_1()
                    .child(Icon::empty().path("icons/key-round.svg").size(theme::z(14.)).text_color(if k.stored { p.label } else { p.tertiary }))
                    .child(v_flex().flex_1().min_w_0().child(div().set(Type::Callout).text_color(p.label).child(name.clone())).child(dim(detail)))
                    .when(k.uses > 0, |d| d.child(dim(format!("used {}×", k.uses))))
                    .child(Button::new(ElementId::from(SharedString::from(format!("key-rm-{name}")))).xsmall().ghost().icon(Icon::empty().path("icons/trash.svg")).on_click(cx.listener(move |this, _, _, cx| {
                        this.cmd(Cmd::Ask { tag: "keys", req: Request::KeyRemove { name: name.clone() } });
                        cx.notify();
                    }))),
            );
        }
        let f = &self.key_inputs;
        col = col
            .child(div().pt_2().set(Type::Headline).text_color(p.label).child("Add a Key"))
            .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&f.name).id("key-name").small())).child(div().flex_1().child(Input::new(&f.host).id("key-host").small())))
            .child(h_flex().gap_2().child(div().flex_1().child(Input::new(&f.env).id("key-env").small())).child(div().flex_1().child(Input::new(&f.secret).id("key-secret").small())))
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("key-add").small().primary().label("Add").on_click(cx.listener(|this, _, window, cx| this.add_key(window, cx))))
                    .child(Button::new("key-github").small().outline().label("Use GitHub CLI Login").on_click(cx.listener(|this, _, _, cx| this.github_from_cli(cx)))),
            );
        if !uses.is_empty() {
            col = col.child(div().pt_2().set(Type::Headline).text_color(p.label).child("Recent Use"));
            for u in uses.iter().take(12) {
                col = col.child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(14.)).set(Type::Callout).text_color(if u.allowed { p.secondary } else { p.failed }).child(if u.allowed { "✓" } else { "✕" }))
                        .child(div().flex_1().min_w_0().set(Type::Code).text_color(p.secondary).truncate().child(format!("{} {}{}", u.method, u.host, u.path))),
                );
            }
        }
        col
    }
}
