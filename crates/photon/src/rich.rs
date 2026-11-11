//! diagrams and maths inside what the agent writes.
//!
//! the markdown view draws prose. a fenced `mermaid` block and display maths
//! (`$$ … $$` or `\[ … \]`) are cut out of the prose here and drawn as
//! pictures by `photon-rich`. anything still being written (a fence that has
//! not closed yet) stays as text until it is complete, so a half-streamed
//! diagram never flashes an error.

/// one stretch of a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    Prose(String),
    Mermaid(String),
    /// display maths, without its delimiters.
    Math(String),
}

fn fence(line: &str) -> Option<(&str, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ticks = rest.len() - rest.trim_start_matches('`').len();
    (ticks >= 3).then(|| (&rest[..ticks], rest[ticks..].trim()))
}

/// cut a reply into prose, diagrams and display maths, in order.
pub fn split(text: &str) -> Vec<Seg> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Seg> = Vec::new();
    let mut prose: Vec<&str> = Vec::new();
    let flush = |prose: &mut Vec<&str>, out: &mut Vec<Seg>| {
        if prose.iter().any(|l| !l.trim().is_empty()) {
            out.push(Seg::Prose(prose.join("\n")));
        }
        prose.clear();
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ticks, info)) = fence(line) {
            // where this block closes: a line of at least as many ticks and nothing else.
            let close = (i + 1..lines.len()).find(|&j| fence(lines[j]).is_some_and(|(t, rest)| t.len() >= ticks.len() && rest.is_empty()));
            let is_mermaid = info.split_whitespace().next().is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid"));
            match close {
                Some(end) if is_mermaid => {
                    flush(&mut prose, &mut out);
                    out.push(Seg::Mermaid(lines[i + 1..end].join("\n")));
                    i = end + 1;
                }
                // any other code is prose as far as we are concerned, and
                // nothing inside it is looked at: `$$` in a shell script is not maths.
                Some(end) => {
                    prose.extend(&lines[i..=end]);
                    i = end + 1;
                }
                // still being written.
                None => {
                    prose.extend(&lines[i..]);
                    i = lines.len();
                }
            }
            continue;
        }
        let t = line.trim();
        // `$$ x $$` on one line.
        if t.len() > 4 && t.starts_with("$$") && t.ends_with("$$") && !t[2..t.len() - 2].contains("$$") {
            flush(&mut prose, &mut out);
            out.push(Seg::Math(t[2..t.len() - 2].trim().to_owned()));
            i += 1;
            continue;
        }
        let closer = match t {
            "$$" => Some("$$"),
            "\\[" => Some("\\]"),
            _ => None,
        };
        if let Some(closer) = closer
            && let Some(end) = (i + 1..lines.len()).find(|&j| lines[j].trim() == closer)
        {
            let body = lines[i + 1..end].join("\n");
            if !body.trim().is_empty() {
                flush(&mut prose, &mut out);
                out.push(Seg::Math(body.trim().to_owned()));
                i = end + 1;
                continue;
            }
        }
        prose.push(line);
        i += 1;
    }
    flush(&mut prose, &mut out);
    out
}

/// does this reply have anything to draw? most don't, and skip all of this.
pub fn has_rich(text: &str) -> bool {
    text.contains("```") || text.contains("$$") || text.contains("\\[")
}

#[cfg(test)]
mod tests {
    use super::{Seg, has_rich, split};

    fn prose(s: &str) -> Seg {
        Seg::Prose(s.to_owned())
    }

    #[test]
    fn plain_prose_is_one_piece_and_is_not_even_looked_at_twice() {
        assert_eq!(split("hello\n\nworld"), [prose("hello\n\nworld")]);
        assert!(!has_rich("hello `code` and $5"));
        assert!(split("").is_empty() && split("\n \n").is_empty());
    }

    #[test]
    fn a_mermaid_block_is_cut_out_with_the_prose_around_it_kept_in_order() {
        let text = "Here is the flow:\n\n```mermaid\ngraph TD\n  A --> B\n```\n\nThat's it.";
        assert_eq!(split(text), [prose("Here is the flow:\n"), Seg::Mermaid("graph TD\n  A --> B".into()), prose("\nThat's it.")]);
        assert_eq!(split("```Mermaid\npie\n```"), [Seg::Mermaid("pie".into())], "the language tag is not case sensitive");
    }

    #[test]
    fn a_diagram_still_being_written_stays_as_text_until_its_fence_closes() {
        let half = "Flow:\n```mermaid\ngraph TD\n  A -->";
        assert_eq!(split(half), [prose(half)]);
        let done = format!("{half} B\n```");
        assert!(matches!(split(&done).as_slice(), [Seg::Prose(_), Seg::Mermaid(m)] if m == "graph TD\n  A --> B"));
    }

    #[test]
    fn other_code_is_left_alone_and_nothing_inside_it_is_mistaken_for_maths() {
        let text = "Run:\n```sh\necho $$\n$$\nx\n$$\n```\ndone";
        assert_eq!(split(text), [prose(text)]);
        let nested = "````md\n```mermaid\ngraph TD\n```\n````";
        assert_eq!(split(nested), [prose(nested)], "a diagram quoted inside a longer fence is an example, not a diagram");
    }

    #[test]
    fn display_maths_is_cut_out_in_each_way_it_is_written() {
        assert_eq!(split("so\n$$\nx = \\frac{a}{b}\n$$\nok"), [prose("so"), Seg::Math("x = \\frac{a}{b}".into()), prose("ok")]);
        assert_eq!(split("$$ e^{i\\pi} + 1 = 0 $$"), [Seg::Math("e^{i\\pi} + 1 = 0".into())]);
        assert_eq!(split("\\[\n\\sum_{i=1}^n i\n\\]"), [Seg::Math("\\sum_{i=1}^n i".into())]);
    }

    #[test]
    fn money_and_unfinished_maths_are_not_maths() {
        assert_eq!(split("it costs $5 and $$ is slang"), [prose("it costs $5 and $$ is slang")]);
        assert_eq!(split("$$\nx = 1"), [prose("$$\nx = 1")], "no closing line yet");
        assert_eq!(split("$$\n\n$$"), [prose("$$\n\n$$")], "nothing between the lines is nothing to draw");
    }
}
