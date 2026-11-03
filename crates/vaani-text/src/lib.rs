//! Rule-based cleanup: instant, always on, runs before any optional AI pass.

/// Phrases speech models produce from silence or noise that should never be typed.
const HALLUCINATIONS: &[&str] = &[
    "thank you.",
    "thanks for watching.",
    "thank you for watching.",
    "you",
    "bye.",
    ".",
    "...",
    "[blank_audio]",
    "[music]",
    "(music)",
    "[silence]",
    "subtitles by the amara.org community",
];

const FILLERS: &[&str] = &["um", "uh", "erm", "uhm", "hmm", "mm"];

#[derive(Debug, Clone)]
pub struct Rules {
    pub remove_fillers: bool,
    /// Add a space after the text so the next dictation doesn't run into it.
    pub trailing_space: bool,
    /// Turn "new line", "comma", "full stop" and similar into what they say.
    pub spoken_commands: bool,
    /// Whole-word replacements, matched without regard to case: (heard, written).
    pub replacements: Vec<(String, String)>,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            trailing_space: true,
            spoken_commands: true,
            replacements: Vec::new(),
        }
    }
}

/// Spoken commands, longest first. Punctuation attaches to the previous word.
const COMMANDS: &[(&[&str], &str)] = &[
    (&["new", "paragraph"], "\n\n"),
    (&["next", "paragraph"], "\n\n"),
    (&["new", "line"], "\n"),
    (&["next", "line"], "\n"),
    (&["full", "stop"], "."),
    (&["question", "mark"], "?"),
    (&["exclamation", "mark"], "!"),
    (&["exclamation", "point"], "!"),
    (&["comma"], ","),
    (&["semicolon"], ";"),
    (&["colon"], ":"),
];

/// Lowercase a token and drop the punctuation a model puts around it ("Line." -> "line").
fn bare(token: &str) -> String {
    token
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase()
}

/// Apply spoken commands to space-separated text.
fn apply_commands(text: &str) -> String {
    let tokens: Vec<&str> = text.split(' ').filter(|t| !t.is_empty()).collect();
    let mut out = String::new();
    let mut cap_next = false;
    let mut i = 0;
    'outer: while i < tokens.len() {
        for (words, symbol) in COMMANDS {
            let n = words.len();
            if i + n <= tokens.len() && (0..n).all(|k| bare(tokens[i + k]) == words[k]) {
                // Drop the punctuation the model already put before the command.
                while out.ends_with([',', '.', ';', ':', '?', '!', ' ']) && !out.is_empty() {
                    out.pop();
                }
                out.push_str(symbol);
                cap_next = symbol.contains('\n') || matches!(*symbol, "." | "?" | "!");
                i += n;
                continue 'outer;
            }
        }
        let tok = tokens[i];
        if !out.is_empty() && !out.ends_with('\n') {
            out.push(' ');
        }
        if cap_next {
            out.push_str(&capitalize_first(tok));
        } else {
            out.push_str(tok);
        }
        cap_next = false;
        i += 1;
    }
    out
}

/// Replace whole words or phrases, ignoring case ("vani" -> "Vaani").
fn apply_replacements(text: &str, pairs: &[(String, String)]) -> String {
    let mut text = text.to_string();
    for (from, to) in pairs {
        let from = from.trim();
        if from.is_empty() {
            continue;
        }
        let lower = text.to_lowercase();
        let needle = from.to_lowercase();
        // Lowercasing can change byte lengths for some scripts; skip those rather than
        // risk cutting a character in half.
        if lower.len() != text.len() {
            text = text.replace(from, to);
            continue;
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (at, _) in lower.match_indices(&needle) {
            if at < last {
                continue;
            }
            let end = at + needle.len();
            let before_ok = text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let after_ok = text[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if before_ok && after_ok {
                out.push_str(&text[last..at]);
                out.push_str(to);
                last = end;
            }
        }
        out.push_str(&text[last..]);
        text = out;
    }
    text
}

/// Clean raw model output. Returns None when there is nothing worth inserting.
pub fn clean(raw: &str, rules: &Rules) -> Option<String> {
    let mut text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if HALLUCINATIONS.iter().any(|h| text.eq_ignore_ascii_case(h)) {
        return None;
    }
    if rules.remove_fillers {
        text = remove_fillers(&text);
    }
    if rules.spoken_commands {
        text = apply_commands(&text);
    }
    if !rules.replacements.is_empty() {
        text = apply_replacements(&text, &rules.replacements);
    }
    let text = text.trim_matches(' ');
    if text.trim().is_empty()
        || text
            .chars()
            .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
            && !text.contains('\n')
    {
        return None;
    }
    let mut out = capitalize_first(text);
    if rules.trailing_space && !out.ends_with('\n') {
        out.push(' ');
    }
    Some(out)
}

fn remove_fillers(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(' ') {
        let bare: String = word
            .trim_matches(|c: char| c.is_ascii_punctuation())
            .to_ascii_lowercase();
        if FILLERS.contains(&bare.as_str()) {
            // Keep sentence-ending punctuation attached to the filler ("um." -> ".").
            if let Some(p) = word.chars().last().filter(|c| matches!(c, '.' | '?' | '!')) {
                if let Some(prev) = out.last_mut() {
                    let trimmed = prev.trim_end_matches([',', '.', '?', '!']).to_string();
                    *prev = format!("{trimmed}{p}");
                }
            }
            continue;
        }
        out.push(word.to_string());
    }
    let joined = out.join(" ");
    // "Hello, , world" -> "Hello, world" after removals.
    joined.replace(", ,", ",").replace(" ,", ",")
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Option<String> {
        clean(
            s,
            &Rules {
                remove_fillers: true,
                trailing_space: false,
                ..Rules::default()
            },
        )
    }

    #[test]
    fn spoken_commands() {
        assert_eq!(
            c("Hello. New line. How are you?"),
            Some("Hello\nHow are you?".into())
        );
        assert_eq!(
            c("first point, new paragraph, second point"),
            Some("First point\n\nSecond point".into())
        );
        assert_eq!(
            c("buy milk comma eggs comma and bread full stop"),
            Some("Buy milk, eggs, and bread.".into())
        );
        assert_eq!(
            c("is it ready question mark yes exclamation mark"),
            Some("Is it ready? Yes!".into())
        );
        // Ordinary words stay.
        assert_eq!(c("The line was new."), Some("The line was new.".into()));
    }

    #[test]
    fn replacements() {
        let r = Rules {
            trailing_space: false,
            replacements: vec![
                ("vani".into(), "Vaani".into()),
                ("my email".into(), "sak@example.com".into()),
            ],
            ..Rules::default()
        };
        assert_eq!(
            clean("I use Vani daily, send it to my email.", &r),
            Some("I use Vaani daily, send it to sak@example.com.".into())
        );
        // Whole words only.
        assert_eq!(clean("Vanilla", &r), Some("Vanilla".into()));
    }

    #[test]
    fn whitespace_and_capitalization() {
        assert_eq!(c("  hello   world. "), Some("Hello world.".into()));
    }

    #[test]
    fn fillers_removed() {
        assert_eq!(
            c("um so I think, uh, we should go"),
            Some("So I think, we should go".into())
        );
        assert_eq!(c("Ummm no. Um."), Some("Ummm no.".into()));
    }

    #[test]
    fn hallucinations_and_empty_dropped() {
        assert_eq!(c("Thank you."), None);
        assert_eq!(c("   "), None);
        assert_eq!(c("..."), None);
        assert_eq!(c("um"), None);
    }

    #[test]
    fn trailing_space() {
        assert_eq!(clean("hi", &Rules::default()), Some("Hi ".into()));
    }

    #[test]
    fn unicode_is_kept() {
        assert_eq!(c("नमस्ते दुनिया"), Some("नमस्ते दुनिया".into()));
    }
}
