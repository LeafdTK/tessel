use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub node: serde_json::Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub landed: u32,
    pub review: u32,
    pub working: u32,
    pub ready: u32,
    pub dropped: u32,
    pub cost_per_landed: f64,
    pub waste: f64,
    pub escaped: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lenses: Vec<Graded>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graded {
    pub lens: String,
    pub found: u32,
    pub refuted: u32,
    pub fixed: u32,
    pub ignored: u32,
}

pub fn format() -> String {
    let kinds: Vec<String> = crate::surface::KINDS.iter().map(|(k, about)| if about.is_empty() { (*k).to_owned() } else { format!("{k} ({about})") }).collect();
    let sources: Vec<String> = crate::surface::SOURCES.iter().map(|(s, about)| format!("{s} ({about})")).collect();
    format!(
        "A tessel view is a JSON file in ~/.tessel/views/<id>.json: {{\"title\": \"…\", \"icon\": \"a lucide icon name\", \"node\": an element}}. \
An element is {{\"k\": kind, \"p\": props, \"c\": children}}. Kinds: {}. \
Any element can take its data live instead of fixed props: put \"bind\": a source in its p, and tessel fills it in as things change, with no agent involved. Sources: {}. \
A bound table gets its columns and rows, bars their items, a metric its value. Keep a view to one screen: a row of a few metrics, then a chart or a table.",
        kinds.join("; "),
        sources.join("; ")
    )
}
