//! what's said, made into what's meant: the noises people make while they
//! talk left out (here, instantly, with no model), and the ask for a model to
//! finish the job (false starts, punctuation, the names of things).

/// sounds, not words: never meant to be written down.
const NOISES: [&str; 16] = ["um", "umm", "uh", "uhh", "uhm", "erm", "er", "hmm", "hm", "mm", "mmm", "mhm", "mm-hmm", "uh-huh", "uh-uh", "ahem"];

/// the noises left out, and what that leaves tidied: no doubled punctuation,
/// no stray commas at the start, each sentence starting with a capital.
pub fn strip_noises(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let bare: String = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-').to_lowercase();
        if NOISES.contains(&bare.as_str()) {
            // the noise's own punctuation ends a sentence it was the end of.
            let end: String = word.chars().rev().take_while(|c| matches!(c, '.' | '?' | '!')).collect();
            if !end.is_empty()
                && let Some(last) = out.last_mut()
            {
                let trimmed = last.trim_end_matches([',', ';', ':']).to_owned();
                if !trimmed.ends_with(['.', '?', '!']) {
                    *last = format!("{trimmed}{}", end.chars().rev().collect::<String>());
                }
            }
            continue;
        }
        out.push(word.to_owned());
    }
    // a sentence that started with a noise starts with a capital after it.
    let mut fixed = Vec::with_capacity(out.len());
    let mut start = true;
    for word in out {
        let mut w = word.trim_start_matches([',', ';']).to_owned();
        if w.is_empty() {
            continue;
        }
        if start {
            let mut c = w.chars();
            if let Some(first) = c.next() {
                w = first.to_uppercase().collect::<String>() + c.as_str();
            }
        }
        start = w.ends_with(['.', '?', '!']);
        fixed.push(w);
    }
    fixed.join(" ")
}

/// what a model is asked, to make a dictated message read as meant. `context`
/// is what the chat is about (its files, its names), for spelling them right.
pub fn tidy_prompt(text: &str, context: &str) -> String {
    format!(
        "Someone dictated this message to a coding agent. Rewrite it as they meant to write it:\n\
- drop false starts, restarts and filler (\"okay so\", \"so on\", \"like\", \"you know\" when it's filler), and words said twice by accident\n\
- fix punctuation and capitals; write file names, commands and identifiers as code is written (hub.rs, tesselctl, camelCase), using the context below for their spelling\n\
- keep their words, meaning, tone and language; don't add, answer, explain, summarize or shorten what they asked\n\
- a question stays a question, a list of asks stays a list of asks\n\
Reply with the rewritten message only, nothing before or after it.\n\n\
Context (what this chat works on):\n{context}\n\n\
Dictated:\n{text}"
    )
}

/// whether a model's rewrite can be used: something, and not wildly longer
/// or shorter than what was said (a model that answered, or gave up).
pub fn plausible(said: &str, rewrite: &str) -> bool {
    let (a, b) = (said.chars().count().max(1) as f32, rewrite.trim().chars().count() as f32);
    b > 0.0 && b <= a * 1.3 + 40.0 && b >= a * 0.35
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noises_go_and_what_is_left_reads_right() {
        assert_eq!(strip_noises("Um, then on copy cleanup. Um, can we be clear?"), "Then on copy cleanup. Can we be clear?");
        assert_eq!(strip_noises("fix the hmm the login uh test"), "Fix the the login test");
        assert_eq!(strip_noises("that's it, mm-hmm."), "That's it.");
        assert_eq!(strip_noises("an umbrella and a hummus"), "An umbrella and a hummus", "only whole words");
        assert_eq!(strip_noises("Um."), "");
    }

    #[test]
    fn a_rewrite_that_answered_instead_is_not_used() {
        assert!(plausible("can you change all of the domains to the new one", "Can you change all the domains to the new one?"));
        assert!(!plausible("fix it", "Sure! Here's how you could fix it: first open the file, then look for the function that…"));
        assert!(!plausible("a long message about many things that matter", ""));
    }
}
