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
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            trailing_space: true,
        }
    }
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
    let text = text.trim();
    if text.is_empty() || text.chars().all(|c| c.is_ascii_punctuation()) {
        return None;
    }
    let mut out = capitalize_first(text);
    if rules.trailing_space {
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
            },
        )
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
