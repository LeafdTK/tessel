use serde::{Deserialize, Serialize};

use crate::{Millis, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Draft,
    Ready,
    Running,
    Proving,
    Review,
    Landing,
    Landed,
    Dropped,
    Planned,
    Checking,
    #[serde(other)]
    Unknown,
}

impl TaskState {
    pub fn active(self) -> bool {
        matches!(self, TaskState::Running | TaskState::Proving | TaskState::Landing)
    }

    pub fn finished(self) -> bool {
        matches!(self, TaskState::Landed | TaskState::Dropped)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskNote {
    pub at_ms: Millis,
    pub by: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spec: String,
    pub repo: String,
    #[serde(default)]
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<TaskNote>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plan: bool,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub proof_cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FileStat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cleared: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<TaskOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub building: Option<usize>,
    pub created_ms: Millis,
    pub updated_ms: Millis,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum TaskAct {
    Ready,
    Draft,
    Land,
    Change { text: String },
    Plan,
    Drop,
    Clear,
    Choose { index: usize },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueInfo {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileStat {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

pub fn land_word(land: &str) -> String {
    match land {
        "main" | "" => "Land on main".into(),
        "pr" => "Open a pull request".into(),
        "keep" => "Keep the branch".into(),
        other => match other.strip_prefix("branch:") {
            Some(b) => format!("Merge into {b}"),
            None => other.to_owned(),
        },
    }
}
