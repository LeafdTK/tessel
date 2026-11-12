//! themes as files: a name, a line about it, and colours (a base palette and
//! the roles it changes, for light and for dark). tessel ships some; any
//! `*.json` in `~/.tessel/themes/` is one more (or replaces one of the same
//! name). applying a theme writes its colours into ui.json's `theme`, and
//! leaves appearance, size, fonts, density and material as they were.

use super::ThemeSpec;
use serde_json::{Map, Value};
use std::path::Path;

pub const SHIPPED: [(&str, &str); 13] = [
    ("tessel", include_str!("themes/tessel.json")),
    ("orchard", include_str!("themes/orchard.json")),
    ("tern", include_str!("themes/tern.json")),
    ("dusk", include_str!("themes/dusk.json")),
    ("paper", include_str!("themes/paper.json")),
    ("moss", include_str!("themes/moss.json")),
    ("ember", include_str!("themes/ember.json")),
    ("glacier", include_str!("themes/glacier.json")),
    ("plum", include_str!("themes/plum.json")),
    ("sand", include_str!("themes/sand.json")),
    ("graphite", include_str!("themes/graphite.json")),
    ("oxblood", include_str!("themes/oxblood.json")),
    ("powder", include_str!("themes/powder.json")),
];

/// the keys of a theme's colours: everything else in ui.json's theme is the person's.
const COLOUR_KEYS: [&str; 3] = ["base", "light", "dark"];

#[derive(Clone, Debug, PartialEq)]
pub struct ThemeFile {
    pub key: String,
    pub name: String,
    pub about: String,
    /// what goes into ui.json's theme: base, light, dark.
    pub colours: Map<String, Value>,
    pub spec: ThemeSpec,
}

pub fn parse(key: &str, text: &str) -> Result<ThemeFile, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{key}: {e}"))?;
    let mut obj = v.as_object().cloned().ok_or_else(|| format!("{key}: a theme is an object"))?;
    let name = obj.remove("name").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| key.to_owned());
    let about = obj.remove("about").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    if let Some(k) = obj.keys().find(|k| !COLOUR_KEYS.contains(&k.as_str())) {
        return Err(format!("{key}: a theme is colours only (name, about, base, light, dark), not \"{k}\""));
    }
    let spec = ThemeSpec::parse(&Value::Object(obj.clone())).map_err(|e| format!("{key}: {e}"))?;
    Ok(ThemeFile { key: key.to_owned(), name, about, colours: obj, spec })
}

/// every theme: tessel's, then the person's (one with a shipped name replaces it).
pub fn all(user_dir: Option<&Path>) -> Vec<ThemeFile> {
    let mut out: Vec<ThemeFile> = SHIPPED.iter().filter_map(|(k, t)| parse(k, t).ok()).collect();
    let Some(dir) = user_dir else { return out };
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let mut mine: Vec<ThemeFile> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension()? != "json" {
                return None;
            }
            let key = path.file_stem()?.to_string_lossy().into_owned();
            parse(&key, &std::fs::read_to_string(&path).ok()?).ok()
        })
        .collect();
    mine.sort_by(|a, b| a.key.cmp(&b.key));
    for t in mine {
        match out.iter_mut().find(|o| o.key == t.key) {
            Some(o) => *o = t,
            None => out.push(t),
        }
    }
    out
}

/// whether a theme is the one these colours came from.
pub fn is_current(t: &ThemeFile, current: &ThemeSpec) -> bool {
    let sorted = |v: &[(super::Token, gpui_kit::Hsla)]| {
        let mut v: Vec<String> = v.iter().map(|(t, c)| format!("{t:?}{c:?}")).collect();
        v.sort();
        v
    };
    t.spec.base.unwrap_or_default() == current.base.unwrap_or_default() && sorted(&t.spec.light) == sorted(&current.light) && sorted(&t.spec.dark_tokens) == sorted(&current.dark_tokens)
}

/// ui.json's theme with these colours in it, and everything else kept.
pub fn applied(existing: Option<&Value>, t: &ThemeFile) -> Value {
    let mut theme = match existing {
        Some(Value::Object(o)) => o.clone(),
        Some(Value::String(base)) => Map::from_iter([("base".to_owned(), Value::String(base.clone()))]),
        _ => Map::new(),
    };
    for k in COLOUR_KEYS {
        theme.remove(k);
    }
    theme.extend(t.colours.clone());
    Value::Object(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_theme_reads_and_is_readable_in_light_and_dark() {
        let all = all(None);
        assert_eq!(all.len(), SHIPPED.len());
        for t in &all {
            assert!(!t.name.is_empty() && !t.about.is_empty(), "{}", t.key);
            for dark in [false, true] {
                let p = t.spec.palette(dark);
                assert_eq!(p.dark, dark, "{} is a {} palette", t.key, if dark { "dark" } else { "light" });
                // the guardrails had little to fix: the theme was made readable.
                let contrast = crate::theme::contrast(p.label, p.window);
                assert!(contrast >= 7.0, "{} {}: text on the sheet is {contrast:.1}:1", t.key, if dark { "dark" } else { "light" });
            }
        }
    }

    #[test]
    fn applying_a_theme_keeps_the_persons_own_settings() {
        let dusk = all(None).into_iter().find(|t| t.key == "dusk").unwrap();
        let before = serde_json::json!({"base": "tern", "appearance": "dark", "size": "large", "density": "compact", "light": {"accent": "#ff0000"}});
        let after = applied(Some(&before), &dusk);
        assert_eq!(after["appearance"], "dark");
        assert_eq!(after["size"], "large");
        assert_eq!(after["density"], "compact");
        assert_eq!(after["base"], "tessel");
        assert_ne!(after["light"]["accent"], "#ff0000", "the old colours are gone");
        assert!(is_current(&dusk, &ThemeSpec::parse(&after).unwrap()));
    }

    #[test]
    fn a_theme_of_my_own_replaces_or_joins_and_a_bad_one_says_why() {
        let dir = std::env::temp_dir().join(format!("tessel-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.json"), r##"{"name": "Mine", "base": "tern", "light": {"accent": "#0a7"}}"##).unwrap();
        std::fs::write(dir.join("paper.json"), r##"{"name": "My paper", "base": "orchard"}"##).unwrap();
        std::fs::write(dir.join("broken.json"), r##"{"base": "tern", "size": "large"}"##).unwrap();
        let all = all(Some(&dir));
        assert!(all.iter().any(|t| t.key == "mine"));
        assert_eq!(all.iter().find(|t| t.key == "paper").unwrap().name, "My paper");
        assert!(!all.iter().any(|t| t.key == "broken"), "not colours only: left out");
        assert!(parse("broken", r#"{"size": "large"}"#).unwrap_err().contains("colours only"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
