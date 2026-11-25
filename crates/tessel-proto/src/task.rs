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

    pub fn word(self) -> &'static str {
        match self {
            TaskState::Draft => "draft",
            TaskState::Ready => "ready",
            TaskState::Running => "running",
            TaskState::Proving => "proving",
            TaskState::Review => "review",
            TaskState::Landing => "landing",
            TaskState::Landed => "landed",
            TaskState::Dropped => "dropped",
            TaskState::Planned => "planned",
            TaskState::Checking => "checking",
            TaskState::Unknown => "unknown",
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureClass>,
    pub created_ms: Millis,
    pub updated_ms: Millis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ProviderTransient,
    Infra,
    EnvBlocked,
    NoDiff,
    VerifyFailed,
    JudgeVeto,
    Timeout,
    Cancelled,
    LandConflict,
    #[serde(other)]
    Unknown,
}

impl FailureClass {
    pub fn retryable(self) -> bool {
        matches!(self, FailureClass::ProviderTransient | FailureClass::Infra)
    }

    pub fn word(self) -> &'static str {
        match self {
            FailureClass::ProviderTransient => "provider_transient",
            FailureClass::Infra => "infra",
            FailureClass::EnvBlocked => "env_blocked",
            FailureClass::NoDiff => "no_diff",
            FailureClass::VerifyFailed => "verify_failed",
            FailureClass::JudgeVeto => "judge_veto",
            FailureClass::Timeout => "timeout",
            FailureClass::Cancelled => "cancelled",
            FailureClass::LandConflict => "land_conflict",
            FailureClass::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    Created,
    Filed,
    Edited,
    Ready,
    Draft,
    Plan,
    Unblocked,
    Dispatched,
    Started,
    TurnEnded,
    Proof,
    SentBack,
    Reviewing,
    Review,
    Approved,
    Asked,
    Options,
    Chose,
    Changed,
    Split,
    Landing,
    Landed,
    OpenedPr,
    Ci,
    Merging,
    Merged,
    Held,
    Escalated,
    SetAside,
    Dropped,
    Cleared,
    Retried,
    Paused,
    Resumed,
    Watchdog,
    Recovered,
    Illegal,
    #[serde(other)]
    Unknown,
}

impl TaskEventKind {
    pub fn word(self) -> &'static str {
        match self {
            TaskEventKind::Created => "created",
            TaskEventKind::Filed => "filed",
            TaskEventKind::Edited => "edited",
            TaskEventKind::Ready => "ready",
            TaskEventKind::Draft => "draft",
            TaskEventKind::Plan => "plan",
            TaskEventKind::Unblocked => "unblocked",
            TaskEventKind::Dispatched => "dispatched",
            TaskEventKind::Started => "started",
            TaskEventKind::TurnEnded => "turn_ended",
            TaskEventKind::Proof => "proof",
            TaskEventKind::SentBack => "sent_back",
            TaskEventKind::Reviewing => "reviewing",
            TaskEventKind::Review => "review",
            TaskEventKind::Approved => "approved",
            TaskEventKind::Asked => "asked",
            TaskEventKind::Options => "options",
            TaskEventKind::Chose => "chose",
            TaskEventKind::Changed => "changed",
            TaskEventKind::Split => "split",
            TaskEventKind::Landing => "landing",
            TaskEventKind::Landed => "landed",
            TaskEventKind::OpenedPr => "opened_pr",
            TaskEventKind::Ci => "ci",
            TaskEventKind::Merging => "merging",
            TaskEventKind::Merged => "merged",
            TaskEventKind::Held => "held",
            TaskEventKind::Escalated => "escalated",
            TaskEventKind::SetAside => "set_aside",
            TaskEventKind::Dropped => "dropped",
            TaskEventKind::Cleared => "cleared",
            TaskEventKind::Retried => "retried",
            TaskEventKind::Paused => "paused",
            TaskEventKind::Resumed => "resumed",
            TaskEventKind::Watchdog => "watchdog",
            TaskEventKind::Recovered => "recovered",
            TaskEventKind::Illegal => "illegal",
            TaskEventKind::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub id: i64,
    pub task_id: String,
    pub seq: i64,
    pub at_ms: Millis,
    pub kind: TaskEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TaskState>,
    pub to: TaskState,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureClass>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackChannel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub private: bool,
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
