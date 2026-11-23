use globset::{Glob, GlobSetBuilder};
use serde_yaml_ng::Value as Yaml;
use std::collections::BTreeMap;
use std::path::Path;
use tessel_proto::proof::{Check, Knowledge, Rule, Rulebook, Severity, Within};

pub const RULES: &str = ".tessel/rules";
pub const KNOWLEDGE: &str = ".tessel/knowledge";
const GUIDANCE: [&str; 4] = ["AGENTS.md", "CLAUDE.md", ".cursor/rules", ".claude/rules"];
const RULE_KEYS: [&str; 9] = ["id", "title", "paths", "check", "severity", "run", "pattern", "within", "must"];

#[derive(Clone, Debug)]
pub struct Config {
    pub verify: Vec<String>,
    pub model: String,
    pub lenses: Vec<String>,
    pub starter: bool,
    pub gate: bool,
}

pub const LENSES: [&str; 5] = ["correctness", "edge-cases", "security", "rules", "scope"];

pub fn config(workspace: &Path, root: &Path) -> Config {
    let (t, _) = crate::setup::merged(workspace, root);
    let proof = t.get("proof").and_then(toml::Value::as_table);
    let get = |k: &str| proof.and_then(|p| p.get(k));
    let list = |v: Option<&toml::Value>| -> Option<Vec<String>> {
        match v? {
            toml::Value::String(s) if !s.trim().is_empty() => Some(vec![s.trim().to_owned()]),
            toml::Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()),
            _ => None,
        }
    };
    Config {
        verify: list(get("verify")).unwrap_or_default(),
        model: get("model").and_then(toml::Value::as_str).unwrap_or("sonnet").to_owned(),
        lenses: list(get("lenses")).filter(|l| !l.is_empty()).unwrap_or_else(|| LENSES.iter().map(|s| (*s).to_owned()).collect()),
        starter: get("starter").and_then(toml::Value::as_bool).unwrap_or(true),
        gate: get("gate").and_then(toml::Value::as_bool).unwrap_or(true),
    }
}

pub fn load(workspace: &Path, root: &Path) -> Rulebook {
    let cfg = config(workspace, root);
    let mut book = Rulebook { verify: cfg.verify.clone(), ..Default::default() };
    let mut rules: BTreeMap<String, Rule> = BTreeMap::new();
    let mut knowledge: BTreeMap<String, Knowledge> = BTreeMap::new();
    if cfg.starter {
        for r in starter() {
            rules.insert(r.id.clone(), r);
        }
    }
    let mut dirs = vec![root];
    if workspace != root {
        dirs.push(workspace);
    }
    for dir in dirs {
        for (path, text) in md_files(&dir.join(RULES)) {
            match parse_rule(&stem(&path), &rel(dir, &path), &text) {
                Ok(r) => {
                    rules.insert(r.id.clone(), r);
                }
                Err(e) => book.problems.push(format!("{}: {e}", rel(dir, &path))),
            }
        }
        for (path, text) in md_files(&dir.join(KNOWLEDGE)) {
            let k = parse_knowledge(&stem(&path), &rel(dir, &path), &text);
            knowledge.insert(k.id.clone(), k);
        }
    }
    for g in GUIDANCE {
        let p = workspace.join(g);
        if p.is_file() {
            book.guidance.push(g.to_owned());
        } else if p.is_dir() {
            let mut files: Vec<String> = md_files(&p).into_iter().map(|(f, _)| rel(workspace, &f)).collect();
            files.sort();
            book.guidance.extend(files);
        }
    }
    book.rules = rules.into_values().collect();
    book.knowledge = knowledge.into_values().collect();
    book
}

fn md_files(dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md" || e == "mdc"))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).to_string_lossy().into_owned()
}

pub fn front(text: &str) -> (Option<&str>, &str) {
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else { return (None, t) };
    let rest = rest.trim_start_matches([' ', '\t']).strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"));
    let Some(rest) = rest else { return (None, t) };
    match rest.find("\n---") {
        Some(end) => {
            let after = &rest[end + 4..];
            let after = after.split_once('\n').map_or("", |(_, b)| b);
            (Some(&rest[..end]), after)
        }
        None => (None, t),
    }
}

fn title_of(body: &str, id: &str) -> String {
    body.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(100).collect())
        .unwrap_or_else(|| id.replace(['-', '_'], " "))
}

fn strings(v: Option<&Yaml>) -> Vec<String> {
    match v {
        Some(Yaml::String(s)) => s.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect(),
        Some(Yaml::Sequence(a)) => a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

fn text(v: Option<&Yaml>) -> Option<String> {
    match v? {
        Yaml::String(s) => Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
        Yaml::Bool(b) => Some(b.to_string()),
        Yaml::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn parse_rule(stem: &str, path: &str, raw: &str) -> Result<Rule, String> {
    let (fm, body) = front(raw);
    let map = match fm {
        Some(fm) => match serde_yaml_ng::from_str::<Yaml>(fm).map_err(|e| format!("its header isn't valid yaml: {e}"))? {
            Yaml::Mapping(m) => m,
            Yaml::Null => Default::default(),
            _ => return Err("its header should be keys like `paths:` and `check:`".into()),
        },
        None => Default::default(),
    };
    let get = |k: &str| map.get(Yaml::String(k.to_owned()));
    for k in map.keys().filter_map(|k| k.as_str()) {
        if !RULE_KEYS.contains(&k) && k != "globs" && k != "description" && k != "alwaysApply" {
            return Err(format!("\"{k}\" isn't something a rule has. it has: {}", RULE_KEYS.join(", ")));
        }
    }
    let id = text(get("id")).unwrap_or_else(|| stem.to_owned());
    let body = body.trim().to_owned();
    let mut paths = strings(get("paths"));
    paths.extend(strings(get("globs")));
    let severity = match text(get("severity")).as_deref() {
        None | Some("block") | Some("error") => Severity::Block,
        Some("warn") | Some("warning") => Severity::Warn,
        Some("note") | Some("info") => Severity::Note,
        Some(other) => return Err(format!("severity \"{other}\" isn't one: block, warn or note")),
    };
    let named = text(get("check"));
    let check = match (named.as_deref(), text(get("run")), text(get("pattern"))) {
        (Some("run") | None, Some(command), _) => Check::Run { command },
        (Some("run"), None, _) => return Err("a `check: run` rule needs `run:` with the command".into()),
        (Some("match") | None, None, Some(pattern)) => {
            regex::Regex::new(&pattern).map_err(|e| format!("its pattern doesn't compile: {e}"))?;
            let within = match text(get("within")).as_deref() {
                None | Some("added") | Some("lines") => Within::Added,
                Some("subjects") | Some("commits") => Within::Subjects,
                Some("paths") | Some("files") => Within::Paths,
                Some(other) => return Err(format!("within \"{other}\" isn't one: added, subjects or paths")),
            };
            let must = match text(get("must")).as_deref() {
                Some("match") | Some("true") => true,
                Some("not-match") | Some("never") | Some("false") => false,
                None => within == Within::Subjects,
                Some(other) => return Err(format!("must \"{other}\" isn't one: match or not-match")),
            };
            Check::Match { pattern, within, must }
        }
        (Some("match"), None, None) => return Err("a `check: match` rule needs `pattern:`".into()),
        (Some("judge") | None, _, _) => {
            if body.is_empty() {
                return Err("it says nothing: write what the rule is under the header".into());
            }
            Check::Judge
        }
        (Some(other), _, _) => return Err(format!("check \"{other}\" isn't one: judge, run or match")),
    };
    let title = text(get("title")).or_else(|| text(get("description"))).unwrap_or_else(|| title_of(&body, &id));
    Ok(Rule { id, title, path: path.to_owned(), paths, check, severity, body, builtin: false })
}

pub fn parse_knowledge(stem: &str, path: &str, raw: &str) -> Knowledge {
    let (fm, body) = front(raw);
    let map = fm.and_then(|f| serde_yaml_ng::from_str::<Yaml>(f).ok()).and_then(|y| y.as_mapping().cloned()).unwrap_or_default();
    let get = |k: &str| map.get(Yaml::String(k.to_owned()));
    let body = body.trim().to_owned();
    let mut paths = strings(get("paths"));
    paths.extend(strings(get("globs")));
    Knowledge { id: stem.to_owned(), title: text(get("title")).unwrap_or_else(|| title_of(&body, stem)), path: path.to_owned(), paths, body }
}

fn builtin(id: &str, title: &str, check: Check, severity: Severity, body: &str) -> Rule {
    Rule { id: id.into(), title: title.into(), path: String::new(), paths: Vec::new(), check, severity, body: body.into(), builtin: true }
}

pub fn starter() -> Vec<Rule> {
    vec![
        builtin(
            "no-secrets",
            "No secrets in the change",
            Check::Match {
                pattern: r"(-----BEGIN [A-Z ]*PRIVATE KEY-----|AKIA[0-9A-Z]{16}|ghp_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{40,}|sk-ant-[A-Za-z0-9_-]{20,}|xox[abpr]-[A-Za-z0-9-]{10,}|(?i)aws_secret_access_key\s*[:=]\s*\S{20,})".into(),
                within: Within::Added,
                must: false,
            },
            Severity::Block,
            "Keys and tokens never go into the repository. Use the environment or a secret store.",
        ),
        builtin(
            "no-skipped-tests",
            "No tests turned off",
            Check::Match { pattern: r"(\b(it|test|describe)\.(only|skip)\(|\bx(it|describe)\(|#\[ignore\]|@pytest\.mark\.skip|\bt\.Skip\()".into(), within: Within::Added, must: false },
            Severity::Warn,
            "A failing test gets fixed, not skipped or focused.",
        ),
        builtin(
            "dependencies-changed",
            "Dependencies changed",
            Check::Match { pattern: r"(^|/)(Cargo\.toml|package\.json|go\.mod|pyproject\.toml|Gemfile|requirements[^/]*\.txt)$".into(), within: Within::Paths, must: false },
            Severity::Note,
            "A new dependency needs a reason the change says out loud.",
        ),
    ]
}

pub fn commit_style(history: &[String]) -> Option<Rule> {
    let conventional = regex::Regex::new(r"^([a-z]+)(\([^)]*\))?!?: \S").ok()?;
    let typed: Vec<&str> = history.iter().filter_map(|s| conventional.captures(s).and_then(|c| c.get(1)).map(|m| m.as_str())).collect();
    if history.len() < 5 || typed.len() * 10 < history.len() * 7 {
        return None;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for t in &typed {
        *counts.entry(t).or_default() += 1;
    }
    let types: Vec<&str> = counts.into_iter().filter(|(_, n)| *n >= 2).map(|(t, _)| t).collect();
    if types.is_empty() {
        return None;
    }
    Some(builtin(
        "commit-style",
        "Commit subjects follow the repository's style",
        Check::Match { pattern: format!(r"^({})(\([^)]*\))?!?: \S", types.join("|")), within: Within::Subjects, must: true },
        Severity::Warn,
        &format!("This repository's commits start with a type ({}), an optional scope, and a colon.", types.join(", ")),
    ))
}

pub fn applies(paths: &[String], changed: &[String]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut b = GlobSetBuilder::new();
    for p in paths {
        if let Ok(g) = Glob::new(p) {
            b.add(g);
        }
        if !p.contains('/')
            && let Ok(g) = Glob::new(&format!("**/{p}"))
        {
            b.add(g);
        }
    }
    let Ok(set) = b.build() else { return false };
    changed.iter().any(|c| set.is_match(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_judge_rule_reads_its_header_and_body() {
        let r = parse_rule(
            "k8s",
            ".tessel/rules/k8s.md",
            "---\nid: k8s-through-reconciler\npaths: [\"services/**/k8s/**\", \"*kube*.go\"]\nseverity: block\n---\n# Kubernetes through the reconciler\n\nNever call Update directly.\n",
        )
        .unwrap();
        assert_eq!(r.id, "k8s-through-reconciler");
        assert_eq!(r.check, Check::Judge);
        assert_eq!(r.title, "Kubernetes through the reconciler");
        assert!(applies(&r.paths, &["services/api/k8s/deploy.go".into()]));
        assert!(applies(&r.paths, &["pkg/kubeclient.go".into()]), "a bare glob matches at any depth");
        assert!(!applies(&r.paths, &["web/app.tsx".into()]));
    }

    #[test]
    fn run_and_match_rules_are_told_apart_by_what_they_have() {
        let run = parse_rule("lint", "", "---\nrun: cargo clippy -- -D warnings\n---\nClippy is clean.").unwrap();
        assert_eq!(run.check, Check::Run { command: "cargo clippy -- -D warnings".into() });
        let subj = parse_rule("commits", "", "---\npattern: '^(feat|fix|chore)(\\(.+\\))?: .{1,50}$'\nwithin: subjects\n---\nShort typed subjects.").unwrap();
        assert!(matches!(subj.check, Check::Match { within: Within::Subjects, must: true, .. }), "a subject pattern is one they must match");
        let banned = parse_rule("no-unwrap", "", "---\npattern: '\\.unwrap\\(\\)'\nseverity: warn\n---\nNo unwrap.").unwrap();
        assert!(matches!(banned.check, Check::Match { within: Within::Added, must: false, .. }), "an added-lines pattern is one they must not match");
        assert_eq!(banned.severity, Severity::Warn);
    }

    #[test]
    fn a_broken_rule_says_what_is_wrong() {
        assert!(parse_rule("x", "", "---\npattern: '('\n---\nbad").unwrap_err().contains("doesn't compile"));
        assert!(parse_rule("x", "", "---\nseverity: maybe\n---\nhm").unwrap_err().contains("severity"));
        assert!(parse_rule("x", "", "---\npaths: a\n---\n").unwrap_err().contains("says nothing"));
        assert!(parse_rule("x", "", "---\nscope: x\n---\nbody").unwrap_err().contains("\"scope\""));
    }

    #[test]
    fn cursor_style_rules_are_read_as_they_are() {
        let r = parse_rule("api", "", "---\ndescription: API handlers validate input\nglobs: src/api/**\nalwaysApply: false\n---\nValidate every field.").unwrap();
        assert_eq!(r.title, "API handlers validate input");
        assert_eq!(r.paths, vec!["src/api/**"]);
    }

    #[test]
    fn the_commit_style_is_learned_from_history() {
        let h: Vec<String> = ["feat: a", "fix(ui): b", "chore: c", "feat: d", "fix: e", "wip", "feat(x): f", "chore: g"].iter().map(|s| (*s).to_owned()).collect();
        let r = commit_style(&h).expect("mostly typed");
        let Check::Match { pattern, .. } = &r.check else { panic!() };
        let re = regex::Regex::new(pattern).unwrap();
        assert!(re.is_match("fix(server): cache it") && !re.is_match("Fixed the thing") && !re.is_match("docs: x"));
        let loose: Vec<String> = ["Add x", "Fix y", "feat: z", "Update w", "more"].iter().map(|s| (*s).to_owned()).collect();
        assert!(commit_style(&loose).is_none(), "no style to hold anyone to");
    }

    #[test]
    fn a_repository_s_rules_and_knowledge_load_with_the_starter_pack() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(RULES)).unwrap();
        std::fs::create_dir_all(root.join(KNOWLEDGE)).unwrap();
        std::fs::write(root.join(RULES).join("k8s.md"), "---\npaths: \"**/k8s/**\"\n---\nThrough the reconciler.").unwrap();
        std::fs::write(root.join(RULES).join("bad.md"), "---\ncheck: dance\n---\nx").unwrap();
        std::fs::write(root.join(KNOWLEDGE).join("tenants.md"), "---\npaths: \"**/tenant*\"\n---\n# Tenant isolation\nEvery query is scoped.").unwrap();
        std::fs::write(root.join("AGENTS.md"), "# agents").unwrap();
        std::fs::create_dir_all(root.join(".tessel")).unwrap();
        std::fs::write(root.join(".tessel/settings.toml"), "[proof]\nverify = \"cargo test\"\n").unwrap();
        let book = load(root, root);
        assert!(book.rules.iter().any(|r| r.id == "k8s" && !r.builtin));
        assert!(book.rules.iter().any(|r| r.id == "no-secrets" && r.builtin));
        assert_eq!(book.knowledge[0].title, "Tenant isolation");
        assert_eq!(book.guidance, vec!["AGENTS.md"]);
        assert_eq!(book.verify, vec!["cargo test"]);
        assert!(book.problems[0].contains("bad.md"));
    }
}
