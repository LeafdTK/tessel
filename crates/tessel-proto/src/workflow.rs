use crate::{Millis, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub say: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Input>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<Trigger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    Cron { at: String },
    Files { folder: String, paths: Vec<String> },
    Label { repo: String, label: String },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerInfo {
    pub workflow: String,
    pub name: String,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ms: Option<crate::Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Trigger::Cron { at } => format!("on a schedule ({at})"),
            Trigger::Files { paths, .. } => format!("when {} change", paths.join(", ")),
            Trigger::Label { repo, label } => format!("when an issue in {repo} is labeled {label}"),
            Trigger::Unknown => "on something this version doesn't know".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub id: String,
    pub label: String,
    pub kind: InputKind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Options>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Select,
    Multiselect,
    Text,
    Long,
    Toggle,
    Date,
    File,
    #[serde(other)]
    Unknown,
}

impl InputKind {
    pub const NAMES: [&'static str; 7] = ["select", "multiselect", "text", "long", "toggle", "date", "file"];

    pub fn named(s: &str) -> Option<Self> {
        Some(match s {
            "select" => Self::Select,
            "multiselect" => Self::Multiselect,
            "text" => Self::Text,
            "long" => Self::Long,
            "toggle" => Self::Toggle,
            "date" => Self::Date,
            "file" => Self::File,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Options {
    List { choices: Vec<Choice> },
    Tool {
        server: String,
        tool: String,
        #[serde(default)]
        args: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep_secs: Option<u64>,
    },
    Ask {
        ask: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep_secs: Option<u64>,
    },
    #[serde(other)]
    Unknown,
}

impl Options {
    pub fn needs(&self) -> Vec<String> {
        let text = match self {
            Options::Tool { args, .. } => args.to_string(),
            Options::Ask { ask, .. } => ask.clone(),
            _ => String::new(),
        };
        crate::workflow::names_in(&text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub name: String,
    pub does: StepDoes,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub doc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum StepDoes {
    Prompt(String),
    Ask(String),
    Shell(String),
    Task(String),
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    pub value: Value,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub workflow: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub steps: Vec<RunStep>,
    pub at: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    pub started_ms: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<SessionId>,
}

impl Run {
    pub fn done(&self) -> bool {
        self.finished_ms.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunStep {
    pub name: String,
    pub state: StepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Waiting,
    Working,
    Asking,
    Done,
    Failed,
    #[serde(other)]
    Unknown,
}

pub const FORMAT: &str = "A tessel workflow is a YAML file: a recipe that runs from a form instead of a chat. Keys:\n\
- name (required), icon (a lucide icon name, like graduation-cap), description (one line), say (phrases that, typed in the new-session box, offer it: [\"study guide\", \"help me study\"])\n\
- title (the run's chat title, may use inputs), folder (where it runs; default a folder of its own under ~/tessel/runs), model (opus, sonnet, haiku or a full id), effort (low, medium, high, xhigh, max)\n\
- inputs: a map of id to {label, type, options, optional, placeholder, default}. types: select, multiselect, text, long, toggle, date, file (select when it has options). options are a list ([a, b] or [{value, label}]), or {tool: server.tool, args: {...}, label: \"{{name}}\", value: \"{{id}}\", keep: 1d} to fill them by calling one of the person's MCP tools directly (fast, no model), or {ask: \"a prompt\"} to have a small model list them. an input whose options mention another input ({{course}}) loads once that one is picked.\n\
- steps: a list. each is a prompt (a string, or {prompt: ...}), {ask: ...} (you ask the person and wait for the answers), or {shell: a command}. a step may add id, name, fresh: true (a new tab, told what came before), doc: true (its final message is the run's document, saved and shown), model, effort.\n\
- or {task: a title, repo: a folder, spec: what to do}: files a task on the board for an agent to do in a workspace of its own, proved before it lands. it's how a workflow that runs on its own changes code.\n\
- on (optional, runs it by itself; its inputs need defaults): {cron: \"0 9 * * 1-5\"} (local time), {files: {in: a folder, paths: [globs]}}, {github: {repo: owner/name, label: a label}} (new issues with it). what started it is {{event.title}}, {{event.body}}, {{event.url}}, {{event.files}}.\n\
- templates in any text: {{input}} is its value, {{input.label}} what the person saw, {{steps.id.text}} an earlier step's final message.\n\
Keep it short: few inputs, few steps, and put what made a good result into the prompts themselves.";

pub fn names_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        let head = name.split('.').next().unwrap_or("").trim().to_owned();
        if !head.is_empty() && head != "steps" && !out.contains(&head) {
            out.push(head);
        }
        rest = &after[end + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_inputs_a_text_mentions_are_found_once_each() {
        assert_eq!(super::names_in("Gather {{course.label}} for {{ exam }}, {{course}} and {{steps.one.text}}"), ["course", "exam"]);
        assert!(super::names_in("no braces {here}").is_empty());
    }
}
