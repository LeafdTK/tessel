//! surfaces: live ui an agent draws, in the chat or in a pane.
//!
//! a surface is a small tree of nodes (`{"k": "progress", "id": "build",
//! "p": {"value": 0.4}, "c": [...]}`), kept as a transcript item so it
//! persists, replays and streams like everything else. agents change it in
//! place: a `put` replaces the tree, a `set` changes one element's props, a
//! `remove` takes it away. what people do to it (a click, a pick) goes back to
//! the agent as a message.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// the most a surface may weigh, serialized. ui, not a data dump.
pub const MAX_SURFACE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SurfaceOp {
    /// draw (or redraw) the whole surface.
    Put {
        surface: String,
        node: Value,
        /// `inline` (in the chat, where it was drawn) or `pane` (a surfaces pane).
        #[serde(default = "inline")]
        region: String,
    },
    /// change one element's props, merged over what it had.
    Set { surface: String, element: String, props: Value },
    Remove { surface: String },
}

fn inline() -> String {
    "inline".into()
}

impl SurfaceOp {
    pub fn surface(&self) -> &str {
        match self {
            Self::Put { surface, .. } | Self::Set { surface, .. } | Self::Remove { surface } => surface,
        }
    }
}

/// merge `props` into the element with `id` somewhere in `node`. false if
/// there is no such element. a prop set to null is removed.
pub fn set_props(node: &mut Value, id: &str, props: &Value) -> bool {
    if node.get("id").and_then(Value::as_str) == Some(id) {
        let Some(obj) = node.as_object_mut() else { return false };
        let p = obj.entry("p").or_insert_with(|| Value::Object(Default::default()));
        if let (Some(p), Some(new)) = (p.as_object_mut(), props.as_object()) {
            for (k, v) in new {
                if v.is_null() {
                    p.remove(k);
                } else {
                    p.insert(k.clone(), v.clone());
                }
            }
        }
        return true;
    }
    match node.get_mut("c").and_then(Value::as_array_mut) {
        Some(children) => children.iter_mut().any(|c| set_props(c, id, props)),
        None => false,
    }
}

/// every kind of element, and the props each understands. the vocabulary is
/// small on purpose: everything in it is drawn in photon's own style, so an
/// agent can't make something that looks out of place.
pub const KINDS: &[(&str, &str)] = &[
    ("column", "children stacked; p: gap"),
    ("row", "children side by side, wrapping when they don't fit; p: gap"),
    ("card", "a card around its children; p: title, note, status (a badge in its corner), tone (the status's)"),
    ("text", "p: text, tone (default|muted|faint|accent|success|warning|error), size (title|headline|body|callout|caption), weight (regular|medium|semibold), mono"),
    ("markdown", "p: text"),
    ("code", "p: text, lang"),
    ("button", "needs id; p: label, icon (lucide), variant (primary|secondary|ghost; one primary per surface at most). a click sends act=click"),
    ("badge", "p: text, tone"),
    ("progress", "p: value (0..1, or none for indeterminate), label"),
    ("shimmer", "a working line; p: text"),
    ("checkbox", "needs id; p: label, checked. a change sends act=change value=true|false"),
    ("choice", "needs id; p: options (list of text), value. a pick sends act=change value=<option>"),
    ("metric", "p: label, value, delta, tone"),
    ("list", "p: items (list of {title, note, icon}). with an id, a click sends act=click value=<index>"),
    ("kv", "p: rows (list of [key, value])"),
    ("icon", "p: name (lucide), tone"),
    ("divider", ""),
    ("spacer", ""),
];

const DEPTH: usize = 8;
const NODES: usize = 400;

/// what's wrong with a surface, one line each. empty is fine.
pub fn validate(node: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let mut count = 0;
    walk(node, "node", 0, &mut count, &mut problems);
    if count > NODES {
        problems.push(format!("{count} elements is too many; keep it under {NODES}"));
    }
    problems
}

fn walk(node: &Value, at: &str, depth: usize, count: &mut usize, problems: &mut Vec<String>) {
    *count += 1;
    if depth > DEPTH {
        problems.push(format!("{at}: nested deeper than {DEPTH}"));
        return;
    }
    let Some(obj) = node.as_object() else {
        problems.push(format!("{at}: expected an element like {{\"k\": \"text\", \"p\": {{\"text\": \"hi\"}}}}"));
        return;
    };
    let k = obj.get("k").and_then(Value::as_str).unwrap_or("");
    if !KINDS.iter().any(|(n, _)| *n == k) {
        let names: Vec<&str> = KINDS.iter().map(|(n, _)| *n).collect();
        problems.push(format!("{at}: \"{k}\" isn't an element. elements: {}", names.join(", ")));
        return;
    }
    if matches!(k, "button" | "checkbox" | "choice") && obj.get("id").and_then(Value::as_str).is_none() {
        problems.push(format!("{at} ({k}): needs an \"id\" so you can tell what was clicked"));
    }
    for key in obj.keys() {
        if !["k", "id", "p", "c"].contains(&key.as_str()) {
            problems.push(format!("{at} ({k}): \"{key}\" isn't a field. an element has k, id, p (props) and c (children)"));
        }
    }
    if let Some(c) = obj.get("c") {
        match c.as_array() {
            Some(children) => {
                for (i, child) in children.iter().enumerate() {
                    walk(child, &format!("{at}.c[{i}]"), depth + 1, count, problems);
                }
            }
            None => problems.push(format!("{at} ({k}): \"c\" is a list of elements")),
        }
    }
}

/// the message an agent receives when someone acts on its surface.
pub fn event_message(surface: &str, element: &str, act: &str, value: &Value) -> String {
    let value = match value {
        Value::Null => String::new(),
        Value::String(s) => format!(" value=\"{}\"", s.replace('"', "&quot;")),
        other => format!(" value='{other}'"),
    };
    format!("<surface-event surface=\"{surface}\" element=\"{element}\" act=\"{act}\"{value}/>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_set_changes_only_the_named_element_and_null_removes_a_prop() {
        let mut n = json!({"k": "column", "c": [
            {"k": "progress", "id": "build", "p": {"value": 0.1, "label": "Building"}},
            {"k": "row", "c": [{"k": "button", "id": "stop", "p": {"label": "Stop"}}]}
        ]});
        assert!(set_props(&mut n, "build", &json!({"value": 0.6, "label": null})));
        assert_eq!(n["c"][0]["p"], json!({"value": 0.6}));
        assert!(set_props(&mut n, "stop", &json!({"label": "Cancel"})));
        assert_eq!(n["c"][1]["c"][0]["p"]["label"], "Cancel");
        assert!(!set_props(&mut n, "nope", &json!({"x": 1})));
    }

    #[test]
    fn mistakes_are_named_with_where_they_are() {
        assert!(validate(&json!({"k": "column", "c": [{"k": "text", "p": {"text": "hi"}}]})).is_empty());
        let p = validate(&json!({"k": "column", "c": [{"k": "buton"}, {"k": "button", "p": {"label": "Go"}}, {"k": "text", "color": "red"}]}));
        assert!(p.iter().any(|e| e.starts_with("node.c[0]") && e.contains("isn't an element")), "{p:?}");
        assert!(p.iter().any(|e| e.starts_with("node.c[1]") && e.contains("needs an \"id\"")), "{p:?}");
        assert!(p.iter().any(|e| e.contains("\"color\" isn't a field")), "{p:?}");
    }

    #[test]
    fn a_surface_item_survives_the_wire() {
        let item = crate::Item {
            id: crate::ItemId::from("surface/x"),
            parent: None,
            seq: 1,
            order: 1,
            created_ms: 0,
            body: crate::ItemBody::Surface { surface: "x".into(), node: json!({"k": "text"}), region: "inline".into() },
        };
        let back: crate::Item = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn ops_read_as_tagged_json_and_events_read_as_one_line() {
        let op: SurfaceOp = serde_json::from_value(json!({"op": "put", "surface": "s", "node": {"k": "text"}})).unwrap();
        assert!(matches!(op, SurfaceOp::Put { ref region, .. } if region == "inline"));
        assert_eq!(event_message("s", "ok", "click", &Value::Null), r#"<surface-event surface="s" element="ok" act="click"/>"#);
        assert!(event_message("s", "pick", "change", &json!("a\"b")).contains("value=\"a&quot;b\""));
    }
}
