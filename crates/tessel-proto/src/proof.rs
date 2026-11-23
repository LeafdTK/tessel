use serde::{Deserialize, Serialize};

use crate::Millis;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Block,
    Warn,
    #[serde(other)]
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Within {
    #[default]
    Added,
    Subjects,
    Paths,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    Judge,
    Run { command: String },
    Match { pattern: String, within: Within, must: bool },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub check: Check,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub holdout: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Knowledge {
    pub id: String,
    pub title: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rulebook {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub knowledge: Vec<Knowledge>,
    #[serde(default)]
    pub guidance: Vec<String>,
    #[serde(default)]
    pub verify: Vec<String>,
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verified {
    pub command: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    pub ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub lens: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default)]
    pub severity: Severity,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reviewer {
    pub lens: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
    pub ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    #[default]
    Proving,
    Proven,
    Concerns,
    Failed,
    #[serde(other)]
    Broken,
}

impl Verdict {
    pub fn done(self) -> bool {
        self != Verdict::Proving
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proof {
    pub started_ms: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<Millis>,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage: String,
    #[serde(default)]
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<Verified>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<Reviewer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<String>,
    #[serde(default)]
    pub files: u32,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overridden: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub risky: bool,
}

impl Proof {
    pub fn blocking(&self) -> usize {
        self.findings.iter().filter(|f| f.severity == Severity::Block && f.confirmed != Some(false)).count() + self.verify.iter().filter(|v| !v.ok).count()
    }

    pub fn standing(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.confirmed != Some(false))
    }
}

impl Proof {
    pub fn report(&self) -> String {
        let head = match self.verdict {
            Verdict::Proving => format!("still proving: {}", self.stage),
            Verdict::Proven => "proven".to_owned(),
            Verdict::Concerns => "proven, with concerns".to_owned(),
            Verdict::Failed => format!("failed: {} blocking", self.blocking()),
            Verdict::Broken => "couldn't prove it".to_owned(),
        };
        let mut out = format!("{head} ({} files, ${:.2})\n", self.files, self.cost_usd);
        if let Some(p) = &self.problem {
            out.push_str(&format!("{p}\n"));
        }
        for v in &self.verify {
            out.push_str(&format!("{} {}\n", if v.ok { "✓" } else { "✗" }, v.command));
            if !v.ok && !v.output.is_empty() {
                for l in v.output.lines() {
                    out.push_str(&format!("    {l}\n"));
                }
            }
        }
        for f in self.standing() {
            let sev = match f.severity {
                Severity::Block => "block",
                Severity::Warn => "warn",
                Severity::Note => "note",
            };
            let at = match (&f.file, f.line) {
                (Some(file), Some(l)) => format!("{file}:{l} "),
                (Some(file), None) => format!("{file} "),
                _ => String::new(),
            };
            let rule = f.rule.as_deref().map(|r| format!(" [{r}]")).unwrap_or_default();
            out.push_str(&format!("{sev} {at}{}{rule}\n", f.title));
            if !f.why.is_empty() {
                out.push_str(&format!("    {}\n", f.why.replace('\n', "\n    ")));
            }
        }
        for r in self.reviewers.iter().filter(|r| r.error.is_some()) {
            out.push_str(&format!("the {} reviewer couldn't finish: {}\n", r.lens, r.error.as_deref().unwrap_or_default()));
        }
        out.trim_end().to_owned()
    }
}

pub fn has_house_rules(dir: &std::path::Path) -> bool {
    dir.ancestors().take(12).any(|d| d.join(".tessel/rules").is_dir())
}

pub const RULE_FORMAT: &str = "House rules live in the repository's .tessel/ folder, one markdown file each, committed like code.\n\
- .tessel/rules/<id>.md: something every change must follow. A header, then the rule in plain words with why, and a short good and bad example:\n\
  ---\n  paths: [\"services/**/k8s/**\"]   (where it applies; leave out for everywhere)\n  severity: block           (block: must not land; warn: should change; note: worth knowing)\n  ---\n\
  A rule with only words is judged by a reviewer, who cites the file and line. Two kinds are checked without a model, and are cheaper:\n\
  run: <command>   (passes when the command exits 0, like a linter)\n\
  pattern: '<regex>' in single quotes, with within: added | subjects | paths   (added lines and paths must not match; commit subjects must)\n\
- .tessel/knowledge/<id>.md: how part of the system works and why, what is allowed and what isn't, with examples. paths: in its header says when it's relevant. Reviewers and agents read it when a change touches those paths.\n\
- .tessel/settings.toml [proof]: verify = [\"fast, reliable commands\"], risk = [\"globs that get every reviewer\"], auto = true to check each turn's change by itself.\n\
Prefer a run or pattern rule when a command or a regex can check it. Keep each rule to one idea.";
