use serde_json::Value;
use serde_yaml_ng::Value as Yaml;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use tessel_proto::{Choice, Input, InputKind, Options, Pick, Step, StepDoes, Workflow};

const TOP: [&str; 10] = ["name", "icon", "description", "say", "title", "folder", "model", "effort", "inputs", "steps"];
const INPUT_KEYS: [&str; 6] = ["label", "type", "optional", "options", "placeholder", "default"];
const STEP_KEYS: [&str; 9] = ["id", "name", "prompt", "ask", "shell", "fresh", "doc", "model", "effort"];

pub fn dir(home: &Path) -> PathBuf {
    home.join("workflows")
}

pub fn parse(id: &str, path: &str, text: &str) -> Result<Workflow, String> {
    let doc: Yaml = serde_yaml_ng::from_str(text).map_err(|e| format!("{e}"))?;
    let Yaml::Mapping(top) = doc else { return Err("a workflow is a set of keys: name, inputs, steps…".into()) };
    let mut problems = Vec::new();
    let mut w = Workflow { id: id.to_owned(), path: path.to_owned(), ..Default::default() };
    for (k, v) in &top {
        let Some(key) = k.as_str() else { continue };
        match key {
            "name" => w.name = text_of(v),
            "icon" => w.icon = Some(text_of(v)),
            "description" => w.description = Some(text_of(v)),
            "say" => w.say = list_of(v),
            "title" => w.title = Some(text_of(v)),
            "folder" => w.folder = Some(text_of(v)),
            "model" => w.model = Some(text_of(v)),
            "effort" => w.effort = Some(text_of(v)),
            "inputs" => w.inputs = inputs(v, &mut problems),
            "steps" => w.steps = steps(v, &mut problems),
            other => problems.push(format!("\"{other}\" isn't something a workflow has. it has: {}", TOP.join(", "))),
        }
    }
    if w.name.trim().is_empty() {
        w.name = id.replace(['-', '_'], " ");
    }
    if w.steps.is_empty() {
        problems.push("it has no steps: give it at least one, like `steps: [\"what to do\"]`".into());
    }
    let declared: Vec<&str> = w.inputs.iter().map(|i| i.id.as_str()).collect();
    let mut texts: Vec<(String, String)> = Vec::new();
    texts.extend(w.title.iter().map(|t| ("title".to_owned(), t.clone())));
    texts.extend(w.folder.iter().map(|t| ("folder".to_owned(), t.clone())));
    for s in &w.steps {
        if let StepDoes::Prompt(t) | StepDoes::Ask(t) | StepDoes::Shell(t) = &s.does {
            texts.push((format!("step \"{}\"", s.name), t.clone()));
        }
    }
    for i in &w.inputs {
        if let Some(o) = &i.options {
            for n in o.needs() {
                texts.push((format!("{}'s options", i.id), format!("{{{{{n}}}}}")));
            }
        }
    }
    for (place, t) in &texts {
        for n in tessel_proto::workflow::names_in(t) {
            if !declared.contains(&n.as_str()) {
                problems.push(format!("{place} mentions {{{{{n}}}}}, but there's no input called {n}"));
            }
        }
    }
    let step_ids: Vec<&str> = w.steps.iter().map(|s| s.id.as_str()).collect();
    for (place, t) in &texts {
        for r in step_refs(t) {
            if !step_ids.contains(&r.as_str()) {
                problems.push(format!("{place} mentions the step \"{r}\", but there's no step with that id"));
            }
        }
    }
    if w.steps.iter().filter(|s| s.doc).count() > 1 {
        problems.push("only one step can be the doc".into());
    }
    problems.dedup();
    w.problems = problems;
    Ok(w)
}

fn text_of(v: &Yaml) -> String {
    match v {
        Yaml::String(s) => s.trim().to_owned(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn list_of(v: &Yaml) -> Vec<String> {
    match v {
        Yaml::Sequence(items) => items.iter().map(text_of).filter(|s| !s.is_empty()).collect(),
        other => vec![text_of(other)].into_iter().filter(|s| !s.is_empty()).collect(),
    }
}

fn json_of(v: &Yaml) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn keep_secs(v: Option<&Yaml>) -> Option<u64> {
    let v = v?;
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let s = text_of(v);
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().ok()?;
    Some(match unit.trim() {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    })
}

fn inputs(v: &Yaml, problems: &mut Vec<String>) -> Vec<Input> {
    let Yaml::Mapping(m) = v else {
        problems.push("inputs are named: `inputs: { course: { label: Class, type: select } }`".into());
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in m {
        let id = text_of(k);
        let mut input = Input { id: id.clone(), label: id.clone(), kind: InputKind::Text, optional: false, options: None, placeholder: None, default: None };
        let mut kind = None;
        match v {
            Yaml::String(label) => input.label = label.clone(),
            Yaml::Mapping(f) => {
                for (fk, fv) in f {
                    let fk = text_of(fk);
                    match fk.as_str() {
                        "label" => input.label = text_of(fv),
                        "type" => {
                            let t = text_of(fv);
                            match InputKind::named(&t) {
                                Some(k) => kind = Some(k),
                                None => problems.push(format!("{id}: \"{t}\" isn't a type. types: {}", InputKind::NAMES.join(", "))),
                            }
                        }
                        "optional" => input.optional = fv.as_bool().unwrap_or(false),
                        "options" => input.options = options(&id, fv, problems),
                        "placeholder" => input.placeholder = Some(text_of(fv)),
                        "default" => input.default = Some(json_of(fv)),
                        other => problems.push(format!("{id}: \"{other}\" isn't something an input has. it has: {}", INPUT_KEYS.join(", "))),
                    }
                }
            }
            Yaml::Null => {}
            _ => problems.push(format!("{id}: give it a label, or its label and type")),
        }
        input.kind = kind.unwrap_or(if input.options.is_some() { InputKind::Select } else { InputKind::Text });
        if matches!(input.kind, InputKind::Select | InputKind::Multiselect) && input.options.is_none() {
            problems.push(format!("{id} is a {}, but has no options to pick from", if input.kind == InputKind::Select { "select" } else { "multiselect" }));
        }
        out.push(input);
    }
    out
}

fn options(id: &str, v: &Yaml, problems: &mut Vec<String>) -> Option<Options> {
    match v {
        Yaml::Sequence(items) => {
            let choices = items
                .iter()
                .filter_map(|c| match c {
                    Yaml::Mapping(m) => {
                        let get = |k: &str| m.get(k).map(text_of).filter(|s| !s.is_empty());
                        let value = get("value").or_else(|| get("label"))?;
                        Some(Choice { label: get("label").unwrap_or_else(|| value.clone()), value, note: get("note") })
                    }
                    other => {
                        let t = text_of(other);
                        (!t.is_empty()).then(|| Choice { value: t.clone(), label: t, note: None })
                    }
                })
                .collect();
            Some(Options::List { choices })
        }
        Yaml::Mapping(m) => {
            let get = |k: &str| m.get(k).map(text_of).filter(|s| !s.is_empty());
            if let Some(tool) = get("tool") {
                let Some((server, name)) = tool.split_once('.') else {
                    problems.push(format!("{id}: a tool is named with its server, like `canvas.list_courses`"));
                    return None;
                };
                return Some(Options::Tool {
                    server: server.to_owned(),
                    tool: name.to_owned(),
                    args: m.get("args").map(json_of).unwrap_or(Value::Null),
                    label: get("label"),
                    value: get("value"),
                    keep_secs: keep_secs(m.get("keep")),
                });
            }
            if let Some(ask) = get("ask") {
                return Some(Options::Ask { ask, keep_secs: keep_secs(m.get("keep")) });
            }
            problems.push(format!("{id}: options are a list, `{{ tool: server.tool }}`, or `{{ ask: \"…\" }}`"));
            None
        }
        _ => {
            problems.push(format!("{id}: options are a list, `{{ tool: server.tool }}`, or `{{ ask: \"…\" }}`"));
            None
        }
    }
}

fn steps(v: &Yaml, problems: &mut Vec<String>) -> Vec<Step> {
    let items: Vec<&Yaml> = match v {
        Yaml::Sequence(s) => s.iter().collect(),
        other => vec![other],
    };
    let mut out = Vec::new();
    for (n, item) in items.into_iter().enumerate() {
        let mut step = Step { id: format!("step{}", n + 1), name: String::new(), does: StepDoes::Unknown, fresh: false, doc: false, model: None, effort: None };
        match item {
            Yaml::String(s) => step.does = StepDoes::Prompt(s.clone()),
            Yaml::Mapping(m) => {
                let mut does = Vec::new();
                for (k, v) in m {
                    let k = text_of(k);
                    match k.as_str() {
                        "id" => step.id = text_of(v),
                        "name" => step.name = text_of(v),
                        "prompt" => does.push(StepDoes::Prompt(text_of(v))),
                        "ask" => does.push(StepDoes::Ask(text_of(v))),
                        "shell" => does.push(StepDoes::Shell(text_of(v))),
                        "fresh" => step.fresh = v.as_bool().unwrap_or(false),
                        "doc" => step.doc = v.as_bool().unwrap_or(false),
                        "model" => step.model = Some(text_of(v)),
                        "effort" => step.effort = Some(text_of(v)),
                        other => problems.push(format!("step {}: \"{other}\" isn't something a step has. it has: {}", n + 1, STEP_KEYS.join(", "))),
                    }
                }
                match does.len() {
                    1 => step.does = does.remove(0),
                    0 => problems.push(format!("step {}: say what it does with `prompt`, `ask` or `shell`", n + 1)),
                    _ => problems.push(format!("step {}: a step does one thing: `prompt`, `ask` or `shell`", n + 1)),
                }
            }
            _ => problems.push(format!("step {}: a step is a prompt, or `{{ prompt: … }}`", n + 1)),
        }
        if step.name.is_empty() {
            step.name = match &step.does {
                StepDoes::Ask(_) => "Questions".to_owned(),
                StepDoes::Shell(c) => c.split_whitespace().next().map_or_else(|| "Run".to_owned(), |w| format!("Run {w}")),
                StepDoes::Prompt(t) => name_from(t),
                StepDoes::Unknown => format!("Step {}", n + 1),
            };
        }
        out.push(step);
    }
    out
}

fn name_from(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().take(3).collect();
    let name = words.join(" ").trim_end_matches(|c: char| !c.is_alphanumeric()).to_owned();
    if name.is_empty() { "Step".to_owned() } else { name }
}

fn step_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let mut parts = after[..end].trim().split('.');
        if parts.next() == Some("steps")
            && let Some(id) = parts.next()
        {
            out.push(id.trim().to_owned());
        }
        rest = &after[end + 2..];
    }
    out
}

pub fn render(text: &str, picked: &BTreeMap<String, Pick>, steps: &HashMap<String, String>) -> String {
    render_with(text, |name| {
        let mut parts = name.split('.');
        let head = parts.next()?;
        let field = parts.next();
        if head == "steps" {
            return steps.get(field?).cloned();
        }
        let p = picked.get(head)?;
        Some(match field {
            Some("label") => p.label.clone(),
            _ => plain(&p.value),
        })
    })
}

pub fn render_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push_str(&lookup(after[..end].trim()).unwrap_or_default());
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

pub fn render_json(v: &Value, picked: &BTreeMap<String, Pick>) -> Value {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.starts_with("{{") && t.ends_with("}}") && t[2..t.len() - 2].find("{{").is_none() {
                let name = t[2..t.len() - 2].trim();
                if !name.contains('.')
                    && let Some(p) = picked.get(name)
                {
                    return match &p.value {
                        Value::String(s) => s.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::String(s.clone())),
                        other => other.clone(),
                    };
                }
            }
            Value::String(render(s, picked, &HashMap::new()))
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| render_json(x, picked)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), render_json(x, picked))).collect()),
        other => other.clone(),
    }
}

pub fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(plain).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

pub fn slug(name: &str) -> String {
    let s = crate::worktree::slug(name);
    if s.is_empty() { "workflow".to_owned() } else { s }
}

fn repo_of(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

pub fn load_all(home: &Path, cwd: Option<&Path>) -> Vec<Workflow> {
    let mut dirs = vec![(dir(home), None)];
    if let Some(repo) = cwd.and_then(repo_of) {
        let name = repo.file_name().map(|n| n.to_string_lossy().into_owned());
        dirs.push((repo.join(".tessel").join("workflows"), name));
    }
    let mut out: Vec<Workflow> = Vec::new();
    for (d, from) in dirs {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"))).collect();
        files.sort();
        for f in files {
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let id = match &from {
                Some(repo) if out.iter().any(|w| w.id == stem) => format!("{repo}:{stem}"),
                _ => stem.clone(),
            };
            let path = f.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            out.push(parse(&id, &path, &text).unwrap_or_else(|e| Workflow { id, path, name: stem.replace(['-', '_'], " "), problems: vec![format!("it can't be read: {e}")], ..Default::default() }));
        }
    }
    out
}

pub fn find(home: &Path, cwd: Option<&Path>, id: &str) -> Option<Workflow> {
    load_all(home, cwd).into_iter().find(|w| w.id == id)
}

pub fn save(home: &Path, name: &str, yaml: &str) -> Result<Workflow, String> {
    let stem = slug(name);
    let d = dir(home);
    let path = d.join(format!("{stem}.yaml"));
    let w = parse(&stem, &path.to_string_lossy(), yaml)?;
    std::fs::create_dir_all(&d).map_err(|e| format!("making {}: {e}", d.display()))?;
    std::fs::write(&path, yaml).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUIDE: &str = r#"
name: Study guide
icon: graduation-cap
say: ["study guide", "help me study"]
title: "Study guide · {{exam.label}}"
folder: ~/School/{{course.label}}
model: opus
inputs:
  course:
    label: Class
    type: select
    options: { tool: canvas.list_courses, label: "{{name}}", value: "{{id}}", keep: 1d }
  exam:
    label: Exam
    options: { tool: canvas.list_assignments, args: { course_id: "{{course}}" } }
  focus: { label: Anything to focus on?, type: text, optional: true }
steps:
  - id: gather
    prompt: Gather everything in {{course.label}} for {{exam.label}}.
  - ask: Ask me up to 3 questions.
  - prompt: "Write it. {{focus}} Use {{steps.gather.text}}"
    doc: true
"#;

    #[test]
    fn a_workflow_file_reads_into_its_inputs_and_steps() {
        let w = parse("study-guide", "/x.yaml", GUIDE).unwrap();
        assert!(w.problems.is_empty(), "{:?}", w.problems);
        assert_eq!(w.name, "Study guide");
        assert_eq!(w.inputs.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["course", "exam", "focus"], "in the order written");
        assert_eq!(w.inputs[1].kind, InputKind::Select, "options make it a select");
        assert!(matches!(&w.inputs[0].options, Some(Options::Tool { server, tool, keep_secs: Some(86_400), .. }) if server == "canvas" && tool == "list_courses"));
        assert_eq!(w.inputs[1].options.as_ref().unwrap().needs(), ["course"]);
        assert_eq!(w.steps.len(), 3);
        assert_eq!((w.steps[0].id.as_str(), w.steps[1].name.as_str(), w.steps[2].doc), ("gather", "Questions", true));
        assert!(matches!(&w.steps[1].does, StepDoes::Ask(_)));
    }

    #[test]
    fn the_smallest_workflow_is_a_name_and_a_prompt() {
        let w = parse("hi", "/x.yaml", "name: Hi\nsteps: say hello").unwrap();
        assert!(w.problems.is_empty(), "{:?}", w.problems);
        assert!(matches!(&w.steps[0].does, StepDoes::Prompt(p) if p == "say hello"));
    }

    #[test]
    fn mistakes_are_said_plainly() {
        let w = parse("bad", "/x.yaml", "nmae: Oops\ninputs:\n  a: { type: dropdown }\nsteps:\n  - prompt: about {{b}}\n    ask: and\n  - prompt: x {{steps.nope.text}}\n").unwrap();
        let all = w.problems.join("\n");
        for needle in ["\"nmae\" isn't something a workflow has", "\"dropdown\" isn't a type", "a step does one thing", "there's no step with that id"] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
        assert!(parse("x", "/x.yaml", "name: [unclosed").is_err(), "bad yaml is an error");
        let missing = parse("m", "/x.yaml", "name: M\nsteps: [\"about {{who}}\"]").unwrap();
        assert!(missing.problems.iter().any(|p| p.contains("no input called who")), "{:?}", missing.problems);
    }

    #[test]
    fn templates_fill_in_values_labels_and_earlier_steps() {
        let mut picked = BTreeMap::new();
        picked.insert("course".to_owned(), Pick { value: Value::String("42".into()), label: "Calculus II".into() });
        picked.insert("topics".to_owned(), Pick { value: serde_json::json!(["limits", "series"]), label: String::new() });
        let mut steps = HashMap::new();
        steps.insert("gather".to_owned(), "the topics".to_owned());
        assert_eq!(render("{{course.label}} ({{course}}): {{topics}} from {{steps.gather.text}}{{missing}}.", &picked, &steps), "Calculus II (42): limits, series from the topics.");
        assert_eq!(render_json(&serde_json::json!({"course_id": "{{course}}", "q": "in {{course.label}}"}), &picked), serde_json::json!({"course_id": 42, "q": "in Calculus II"}));
    }

    #[test]
    fn workflows_are_found_in_tessels_home_and_saved_there() {
        let tmp = tempfile::tempdir().unwrap();
        let saved = save(tmp.path(), "Study guide", GUIDE).unwrap();
        assert_eq!(saved.id, "study-guide");
        assert!(tmp.path().join("workflows/study-guide.yaml").is_file());
        let all = load_all(tmp.path(), None);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Study guide");
        assert!(save(tmp.path(), "Broken", "name: [").is_err(), "nothing unreadable is written");
        assert!(!tmp.path().join("workflows/broken.yaml").exists());
    }
}
