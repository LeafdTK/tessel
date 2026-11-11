//! taste, enforced.
//!
//! a layout can say anything, but what it says is pulled onto tessel's
//! scales before it is drawn: spacing onto the 4 point grid, corners onto the
//! named radii (and concentric with what they sit in), type onto its sizes and
//! three weights. what can't be fixed quietly is reported as a warning. a
//! layout that wants to break the rules on purpose says `"freestyle": true`
//! and is drawn exactly as written.
//!
//! colour is the other half and happens while drawing: text on a background
//! the theme didn't plan for gets inks re-derived for that background
//! (`Palette::on`), so nothing can be unreadable by accident.

use super::{Kind, Len, Node, Paint, Style, Token, Ui, Variant};

/// spacing: orchard's 4 point grid, plus 6 and 20 which tessel's own layouts use.
pub const SPACE: [f32; 13] = [0.0, 2.0, 4.0, 6.0, 8.0, 12.0, 16.0, 20.0, 24.0, 32.0, 40.0, 48.0, 64.0];
/// corner radii: orchard's named ones and the small ones controls use.
pub const RADIUS: [f32; 10] = [0.0, 4.0, 6.0, 8.0, 10.0, 14.0, 18.0, 22.0, 28.0, 9999.0];
/// type sizes, from apple's mac scale up to display.
pub const TYPE: [f32; 11] = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 17.0, 20.0, 24.0, 28.0, 34.0];
/// regular, medium, semibold.
pub const WEIGHT: [f32; 3] = [450.0, 550.0, 650.0];
/// below this, text can't be read; it is only for things that are leaving.
pub const OPACITY_FLOOR: f32 = 0.4;

/// what the pass did and what it couldn't fix.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// things that will look wrong. the layout is still drawn.
    pub warnings: Vec<String>,
    /// values moved onto a scale, as "where key: from → to".
    pub adjusted: Vec<String>,
}

fn nearest(v: f32, scale: &[f32]) -> f32 {
    // halfway rounds up: 7 on a 6, 8 scale is 8.
    scale.iter().copied().min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()).then(b.total_cmp(a))).unwrap_or(v)
}

fn fmt(v: f32) -> String {
    if v >= 9999.0 {
        "full".into()
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

/// pull a layout onto the scales and look for what will read badly.
pub fn harmonize(ui: &mut Ui) -> Report {
    let mut r = Report::default();
    if ui.freestyle {
        return r;
    }
    walk(&mut ui.root, None, false, &mut r);

    let nodes = ui.nodes();
    let primaries = nodes.iter().filter(|n| n.kind == Kind::Button && n.props.variant == Variant::Primary && n.when.is_empty()).count();
    if primaries > 1 {
        r.warnings.push(format!("{primaries} primary buttons are always on screen. one view gets one; make the rest \"ghost\" or show them only \"when\" they apply"));
    }
    // panes have headers, and a pane header is a title bar: it moves the window,
    // and the one under the window's buttons makes room for them.
    let panes = nodes.iter().any(|n| n.kind == Kind::Panes);
    if !panes && !nodes.iter().any(|n| n.style.drag) {
        r.warnings.push("nothing has \"drag\": true, so there is no title bar to move the window by. give the top row of the layout \"drag\": true".into());
    }
    let left_drawer = nodes.iter().any(|n| n.kind == Kind::Drawer && n.props.side == super::Side::Left);
    if !left_drawer && !panes && !nodes.iter().any(|n| n.style.inset.is_some()) {
        r.warnings.push("nothing leaves room for the window's close/minimise/zoom buttons at the top left. give the top row \"inset\": true".into());
    }
    let conversation = ui.has_module(super::Module::Conversation);
    if !conversation && !nodes.iter().any(|n| n.kind == Kind::Input) {
        r.warnings.push("there is no \"input\", so there is no way to message the agent".into());
    }
    if !conversation && !nodes.iter().any(|n| n.kind == Kind::Transcript) {
        r.warnings.push("there is no \"transcript\", so the conversation can't be seen".into());
    }
    if nodes.iter().any(|n| [n.style.bg, n.style.color, n.style.border_color, n.style.hover].into_iter().flatten().any(|p| matches!(p, Paint::Color(_)))) {
        r.warnings.push("some colours are hex values. they don't follow light and dark or the theme; colour roles (\"raised\", \"muted\", \"accent/20\") do".into());
    }
    r
}

/// a node that draws a surface: a fill, or an edge all the way round. (a
/// toolbar with a line under it is not a surface and gets no corners.)
fn is_surface(s: &Style) -> bool {
    s.bg.is_some_and(|b| b != Paint::Transparent) || s.border.is_some_and(|b| b.top && b.right && b.bottom && b.left)
}

fn is_card(s: &Style) -> bool {
    s.bg.is_some_and(|b| b != Paint::Transparent) && s.border.is_some_and(|b| b.top && b.right && b.bottom && b.left) && s.radius.unwrap_or(0.0) > 0.0
}

/// the room between a container's edge and its children, for concentric corners.
fn inset_of(s: &Style) -> f32 {
    let sides = [s.pt.or(s.py), s.pr.or(s.px), s.pb.or(s.py), s.pl.or(s.px)];
    let given: Vec<f32> = sides.iter().map(|v| v.or(s.p).unwrap_or(0.0)).collect();
    given.into_iter().fold(f32::MAX, f32::min)
}

/// `outer`: the radius and inset of the rounded surface this node sits directly in.
fn walk(n: &mut Node, outer: Option<(f32, f32)>, in_card: bool, r: &mut Report) {
    let at = n.path.clone();
    let s = &mut n.style;

    for (key, slot) in [("p", &mut s.p), ("px", &mut s.px), ("py", &mut s.py), ("pt", &mut s.pt), ("pr", &mut s.pr), ("pb", &mut s.pb), ("pl", &mut s.pl), ("gap", &mut s.gap)] {
        if let Some(v) = *slot {
            let to = nearest(v.max(0.0), &SPACE);
            if to != v {
                r.adjusted.push(format!("{at} {key}: {} → {} (the spacing scale is {})", fmt(v), fmt(to), SPACE.map(fmt).join(", ")));
                *slot = Some(to);
            }
        }
    }
    for (key, slot) in [("w", &mut s.w), ("h", &mut s.h)] {
        if let Some(Len::Px(v)) = *slot {
            let to = (v / 4.0).round() * 4.0;
            if to != v {
                r.adjusted.push(format!("{at} {key}: {} → {} (sizes are multiples of 4)", fmt(v), fmt(to)));
                *slot = Some(Len::Px(to));
            }
        }
    }
    for (key, slot) in [("min_w", &mut s.min_w), ("max_w", &mut s.max_w), ("min_h", &mut s.min_h), ("max_h", &mut s.max_h)] {
        if let Some(v) = *slot {
            let to = (v / 4.0).round() * 4.0;
            if to != v {
                r.adjusted.push(format!("{at} {key}: {} → {} (sizes are multiples of 4)", fmt(v), fmt(to)));
                *slot = Some(to);
            }
        }
    }
    if let Some(super::Font::Size(v)) = s.font {
        let to = nearest(v, &TYPE);
        if to != v {
            r.adjusted.push(format!("{at} font: {} → {} (type sizes are {})", fmt(v), fmt(to), TYPE.map(fmt).join(", ")));
            s.font = Some(super::Font::Size(to));
        }
    }
    if let Some(w) = s.weight {
        let to = nearest(w.0, &WEIGHT);
        if to != w.0 {
            r.adjusted.push(format!("{at} weight: {} → {} (weights are regular 450, medium 550, semibold 650)", fmt(w.0), fmt(to)));
            s.weight = Some(gpui_kit::FontWeight(to));
        }
    }
    if let Some(o) = s.opacity
        && o < OPACITY_FLOOR
    {
        r.adjusted.push(format!("{at} opacity: {} → {} (text under that can't be read)", fmt(o), fmt(OPACITY_FLOOR)));
        s.opacity = Some(OPACITY_FLOOR);
    }

    // corners: onto the scale, then concentric with the surface it sits in.
    if let Some(v) = s.radius {
        let to = nearest(v, &RADIUS);
        if to != v {
            r.adjusted.push(format!("{at} radius: {} → {} (radii are none 0, tile 8, control 10, card 14, panel 18, widget 22, full)", fmt(v), fmt(to)));
            s.radius = Some(to);
        }
    }
    if n.kind != Kind::Drawer
        && is_surface(s)
        && let Some((outer_r, inset)) = outer
        && outer_r > 0.0
        && outer_r < 9999.0
    {
        let fit = (outer_r - inset).max(4.0);
        match s.radius {
            None => s.radius = Some(fit),
            Some(own) if own > fit && inset > 0.0 => {
                r.adjusted.push(format!("{at} radius: {} → {} (inside a {} corner {} in, a corner is {} so the two stay concentric)", fmt(own), fmt(fit), fmt(outer_r), fmt(inset), fmt(fit)));
                s.radius = Some(fit);
            }
            _ => {}
        }
    }

    // a card that floats (a shadow) on another is elevation that means something.
    if in_card && is_card(s) && !s.shadow {
        r.warnings.push(format!("{at} is a bordered card inside another bordered card. nest cards only when the elevation means something; use a hairline or a \"sunken\" fill instead"));
    }
    for paint in [s.bg, s.color, s.border_color].into_iter().flatten() {
        if let Paint::Token(t @ (Token::Warning | Token::Error | Token::Success), _) = paint {
            let name = match t {
                Token::Warning => "warning",
                Token::Error => "error",
                _ => "success",
            };
            r.warnings.push(format!("{at} uses \"{name}\" as decoration. status colours mean that state; for colour without meaning use \"accent\""));
        }
    }
    if n.kind == Kind::Transcript && !s.grow && s.h.is_none() {
        r.adjusted.push(format!("{at} grow: false → true (a transcript with no height shows nothing)"));
        s.grow = true;
    }

    let card = in_card || is_card(s);
    let here = s.radius.filter(|_| is_surface(s) || s.clip).map(|rad| (rad, inset_of(s)));
    for c in &mut n.children {
        walk(c, here, card, r);
    }
}

/// what a check of a layout file found, as text for a person or an agent.
pub fn check_text(result: &Result<Ui, String>) -> String {
    match result {
        Err(e) => format!("error: {e}\n\nthe window keeps the last layout that loaded.\n"),
        Ok(ui) if ui.freestyle => "ok (freestyle: drawn exactly as written, nothing adjusted or checked)\n".into(),
        Ok(ui) => {
            let r = &ui.report;
            let mut out = String::from(if r.warnings.is_empty() { "ok\n" } else { "ok, with warnings\n" });
            for w in &r.warnings {
                out.push_str(&format!("\nwarning: {w}"));
            }
            if !r.adjusted.is_empty() {
                out.push_str("\n\nadjusted onto tessel's scales (write these values to stop seeing this):");
                for a in &r.adjusted {
                    out.push_str(&format!("\n  {a}"));
                }
            }
            out.push('\n');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    #[test]
    fn the_shipped_presets_are_already_in_tune() {
        for (name, text) in super::super::PRESETS {
            let ui = parse(text).unwrap();
            assert!(ui.report.adjusted.is_empty(), "{name} needed adjusting: {:?}", ui.report.adjusted);
            assert!(ui.report.warnings.is_empty(), "{name} has warnings: {:?}", ui.report.warnings);
        }
    }

    #[test]
    fn values_land_on_the_scales_and_say_so() {
        let ui = parse(
            r#"{"root": {"type": "column", "drag": true, "inset": true, "p": 13, "gap": 7, "radius": 15, "w": 301, "children": [
                {"type": "text", "text": "hi", "font": 16, "weight": 500, "opacity": 0.1},
                {"type": "transcript"}, {"type": "input"}
            ]}}"#,
        )
        .unwrap();
        let s = &ui.root.style;
        assert_eq!((s.p, s.gap, s.radius, s.w), (Some(12.0), Some(8.0), Some(14.0), Some(Len::Px(300.0))));
        let t = &ui.root.children[0].style;
        assert_eq!(t.font, Some(super::super::Font::Size(17.0)));
        assert_eq!(t.weight.map(|w| w.0), Some(550.0));
        assert_eq!(t.opacity, Some(OPACITY_FLOOR));
        assert!(ui.root.children[1].style.grow, "a transcript always gets room");
        assert!(ui.report.adjusted.iter().any(|a| a.contains("root p: 13 → 12")), "{:?}", ui.report.adjusted);
        assert!(ui.report.warnings.is_empty(), "{:?}", ui.report.warnings);
    }

    #[test]
    fn corners_inside_corners_are_concentric() {
        let ui = parse(
            r#"{"root": {"type": "column", "drag": true, "inset": true, "children": [
                {"type": "column", "bg": "raised", "border": true, "radius": "panel", "p": 8, "children": [
                    {"type": "row", "bg": "sunken"},
                    {"type": "row", "bg": "sunken", "radius": "panel"},
                    {"type": "row"}
                ]},
                {"type": "transcript"}, {"type": "input"}
            ]}}"#,
        )
        .unwrap();
        let card = &ui.root.children[0];
        assert_eq!(card.children[0].style.radius, Some(10.0), "18 outside, 8 in: 10");
        assert_eq!(card.children[1].style.radius, Some(10.0), "too round for where it sits");
        assert_eq!(card.children[2].style.radius, None, "nothing to round: no surface");
    }

    #[test]
    fn what_reads_badly_is_warned_about() {
        let ui = parse(
            r##"{"root": {"type": "column", "children": [
                {"type": "column", "bg": "raised", "border": true, "radius": "card", "children": [
                    {"type": "column", "bg": "sunken", "border": true, "radius": "card"}
                ]},
                {"type": "text", "text": "!", "color": "error"},
                {"type": "button", "action": "send", "icon": "arrow-up", "variant": "primary"},
                {"type": "button", "action": "attach", "icon": "plus", "variant": "primary"},
                {"type": "row", "bg": "#ff00ff"}
            ]}}"##,
        )
        .unwrap();
        let w = ui.report.warnings.join("\n");
        for expected in ["inside another bordered card", "\"error\" as decoration", "2 primary buttons", "title bar", "close/minimise/zoom", "no \"input\"", "no \"transcript\"", "hex"] {
            assert!(w.contains(expected), "missing {expected:?} in:\n{w}");
        }
    }

    #[test]
    fn freestyle_is_drawn_exactly_as_written() {
        let ui = parse(r#"{"freestyle": true, "root": {"type": "column", "p": 13, "radius": 15, "children": [{"type": "text", "text": "x", "opacity": 0.1}]}}"#).unwrap();
        assert_eq!((ui.root.style.p, ui.root.style.radius), (Some(13.0), Some(15.0)));
        assert_eq!(ui.root.children[0].style.opacity, Some(0.1));
        assert!(ui.report.warnings.is_empty() && ui.report.adjusted.is_empty());
        assert!(check_text(&Ok(ui)).contains("freestyle"));
    }
}
