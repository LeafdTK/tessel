use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tessel_proto::proof::{Finding, Proof, Severity};
use tessel_proto::views::Graded;

pub type Book = BTreeMap<String, BTreeMap<String, Graded>>;

fn file(home: &Path) -> PathBuf {
    home.join("grades.json")
}

pub fn load(home: &Path) -> Book {
    std::fs::read(file(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(home: &Path, book: &Book) {
    if let Ok(b) = serde_json::to_vec_pretty(book) {
        let tmp = file(home).with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, file(home));
        }
    }
}

fn lenses(f: &Finding) -> impl Iterator<Item = &str> {
    f.lens.split(" · ").map(str::trim).filter(|l| !l.is_empty())
}

fn weighty(f: &Finding) -> bool {
    f.severity != Severity::Note && f.confirmed != Some(false)
}

fn same(a: &Finding, b: &Finding) -> bool {
    a.file == b.file
        && (a.rule.is_some() && a.rule == b.rule
            || a.title.eq_ignore_ascii_case(&b.title)
            || match (a.line, b.line) {
                (Some(x), Some(y)) => x.abs_diff(y) <= 3 && lenses(a).any(|l| lenses(b).any(|m| m == l)),
                _ => false,
            })
}

pub fn proved(book: &mut Book, repo: &str, before: Option<&Proof>, now: &Proof) {
    let g = book.entry(repo.to_owned()).or_default();
    for f in &now.findings {
        for l in lenses(f) {
            let e = g.entry(l.to_owned()).or_insert_with(|| Graded { lens: l.to_owned(), ..Default::default() });
            e.found += 1;
            if f.confirmed == Some(false) {
                e.refuted += 1;
            }
        }
    }
    let Some(before) = before else { return };
    if before.fingerprint == now.fingerprint {
        return;
    }
    for f in before.findings.iter().filter(|f| weighty(f)) {
        if !now.findings.iter().any(|n| n.confirmed != Some(false) && same(f, n)) {
            for l in lenses(f) {
                if let Some(e) = g.get_mut(l) {
                    e.fixed += 1;
                }
            }
        }
    }
}

pub fn landed(book: &mut Book, repo: &str, last: &Proof) {
    let g = book.entry(repo.to_owned()).or_default();
    for f in last.findings.iter().filter(|f| weighty(f)) {
        for l in lenses(f) {
            if let Some(e) = g.get_mut(l) {
                e.ignored += 1;
            }
        }
    }
}

pub fn all(book: &Book) -> Vec<Graded> {
    let mut sum: BTreeMap<String, Graded> = BTreeMap::new();
    for g in book.values().flat_map(|r| r.values()) {
        let e = sum.entry(g.lens.clone()).or_insert_with(|| Graded { lens: g.lens.clone(), ..Default::default() });
        e.found += g.found;
        e.refuted += g.refuted;
        e.fixed += g.fixed;
        e.ignored += g.ignored;
    }
    let mut out: Vec<Graded> = sum.into_values().collect();
    out.sort_by(|a, b| b.found.cmp(&a.found).then(a.lens.cmp(&b.lens)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(lens: &str, file: &str, line: u32, title: &str, confirmed: Option<bool>) -> Finding {
        Finding { lens: lens.into(), rule: None, file: Some(file.into()), line: Some(line), severity: Severity::Warn, title: title.into(), why: String::new(), confirmed }
    }

    #[test]
    fn a_finding_that_goes_away_after_a_fix_round_counts_as_fixed_and_a_refuted_one_as_noise() {
        let mut book = Book::new();
        let first = Proof { fingerprint: "a".into(), findings: vec![f("correctness · scope", "a.rs", 10, "Off by one", Some(true)), f("security", "b.rs", 3, "Leaks the token", Some(false)), f("edge-cases", "c.rs", 7, "Empty input", None)], ..Default::default() };
        proved(&mut book, "/r", None, &first);
        let second = Proof { fingerprint: "b".into(), findings: vec![f("edge-cases", "c.rs", 8, "Empty list panics", None)], ..Default::default() };
        proved(&mut book, "/r", Some(&first), &second);
        let g = |l: &str| book["/r"][l].clone();
        assert_eq!((g("correctness").found, g("correctness").fixed), (1, 1));
        assert_eq!(g("scope").fixed, 1, "every lens that saw it gets the credit");
        assert_eq!((g("security").refuted, g("security").fixed), (1, 0), "refuted isn't fixed");
        assert_eq!((g("edge-cases").found, g("edge-cases").fixed), (2, 0), "still there a line later, worded differently: not fixed");
        landed(&mut book, "/r", &second);
        assert_eq!(book["/r"]["edge-cases"].ignored, 1, "landed with it standing");
        proved(&mut book, "/r", Some(&second), &second);
        assert_eq!(book["/r"]["edge-cases"].fixed, 0, "the same proof again proves nothing");
        let total = all(&book);
        assert_eq!(total[0].lens, "edge-cases");
    }
}
