//! Rule-based cleanup: instant, always on, runs before any optional AI pass.

pub mod hindi;

/// Phrases speech models produce from silence or noise that should never be typed.
const HALLUCINATIONS: &[&str] = &[
    "thank you.",
    "thanks for watching.",
    "thanks for watching!",
    "thank you for watching.",
    "thank you for watching!",
    "please subscribe.",
    "like and subscribe.",
    "you",
    "bye.",
    ".",
    "...",
    "[blank_audio]",
    "[music]",
    "(music)",
    "[silence]",
    "[ silence ]",
    "(silence)",
    "[no speech]",
    "subtitles by the amara.org community",
    "धन्यवाद।",
];

/// Short words models also invent from a key click or a breath ("Yeah.", "You", "Okay.").
/// They are real words, so they are only dropped when the recording held almost no speech;
/// see [`is_phantom`].
const PHANTOMS: &[&str] = &[
    "yeah",
    "yes",
    "yep",
    "you",
    "okay",
    "ok",
    "so",
    "oh",
    "ah",
    "uh",
    "um",
    "hmm",
    "mm",
    "mhm",
    "mm-hmm",
    "uh-huh",
    "right",
    "bye",
    "hi",
    "i",
    "the",
    "and",
    "thanks",
    "thank you",
    "thank you so much",
    "हाँ",
    "हां",
];

/// True when the whole transcript is one of the short words models invent from noise.
pub fn is_phantom(raw: &str) -> bool {
    let bare = raw
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c == '।' || c.is_whitespace())
        .to_lowercase();
    PHANTOMS.contains(&bare.as_str())
}

const FILLERS: &[&str] = &["um", "uh", "erm", "uhm", "hmm", "mm"];

#[derive(Debug, Clone)]
pub struct Rules {
    pub remove_fillers: bool,
    /// Add a space after the text so the next dictation doesn't run into it.
    pub trailing_space: bool,
    /// Turn "new line", "comma", "full stop" and similar into what they say.
    pub spoken_commands: bool,
    /// When the dictation ends with a full stop.
    pub full_stop: FullStop,
    /// Whole-word replacements, matched without regard to case: (heard, written).
    pub replacements: Vec<(String, String)>,
    /// Trigger phrase -> saved text. Saying only the trigger inserts just the text.
    pub snippets: Vec<(String, String)>,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            trailing_space: true,
            spoken_commands: true,
            full_stop: FullStop::Smart,
            replacements: Vec::new(),
            snippets: Vec::new(),
        }
    }
}

/// Whether a dictation keeps the full stop the speech model puts at its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FullStop {
    /// Leave it off a single sentence or a fragment ("sounds good"), keep it when the
    /// dictation has several sentences.
    #[default]
    Smart,
    /// Keep whatever the model wrote.
    Keep,
    /// Never end with a full stop.
    Never,
}

/// True when the speaker ended the dictation by saying "full stop" themselves.
pub fn ends_with_spoken_stop(raw: &str) -> bool {
    let words: Vec<String> = raw.split_whitespace().rev().take(2).map(bare).collect();
    words.len() == 2 && words[0] == "stop" && words[1] == "full"
}

/// Apply the full-stop setting to the end of `text`, keeping trailing whitespace as it is.
/// `spoken` means the speaker said "full stop", which is always kept. Question marks,
/// exclamation marks and ellipses are never touched.
pub fn apply_full_stop(text: &str, mode: FullStop, spoken: bool) -> String {
    let body = text.trim_end();
    let tail = &text[body.len()..];
    if spoken || mode == FullStop::Keep || body.ends_with("..") || body.ends_with('…') {
        return text.to_string();
    }
    let Some(stem) = body.strip_suffix('.').or_else(|| body.strip_suffix('।')) else {
        return text.to_string();
    };
    if mode == FullStop::Smart {
        let several = stem.contains('\n')
            || stem.contains('।')
            || [". ", "? ", "! "].iter().any(|b| stem.contains(b));
        if several {
            return text.to_string();
        }
    }
    format!("{}{tail}", stem.trim_end())
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

/// Words that undo the previous dictation instead of being typed.
const SCRATCH: &[&str] = &["scratch that", "delete that", "undo that"];

/// True when the whole dictation is "scratch that" (or "delete that", "undo that").
pub fn is_scratch(raw: &str) -> bool {
    let bare: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    SCRATCH.contains(&bare.as_str())
}

fn apply_snippets(text: &str, snippets: &[(String, String)]) -> String {
    let bare = text
        .trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .trim_start();
    for (trigger, body) in snippets {
        if !trigger.trim().is_empty() && bare.eq_ignore_ascii_case(trigger.trim()) {
            return body.clone();
        }
    }
    apply_replacements(text, snippets)
}

/// When a dictation is corrected by hand, find the one small change worth remembering:
/// up to three words replaced by up to three words ("vani" -> "Vaani"). Bigger edits are
/// rewrites, not spelling fixes, and return None.
pub fn learn(old: &str, new: &str) -> Option<(String, String)> {
    let strip = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let a: Vec<String> = old
        .split_whitespace()
        .map(strip)
        .filter(|w| !w.is_empty())
        .collect();
    let b: Vec<String> = new
        .split_whitespace()
        .map(strip)
        .filter(|w| !w.is_empty())
        .collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let from = &a[pre..a.len() - suf];
    let to = &b[pre..b.len() - suf];
    if from.is_empty() || to.is_empty() || from.len() > 3 || to.len() > 3 {
        return None;
    }
    let (from, to) = (from.join(" "), to.join(" "));
    (from != to).then_some((from, to))
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
    text = apply_full_stop(
        &text,
        rules.full_stop,
        rules.spoken_commands && ends_with_spoken_stop(raw),
    );
    if !rules.replacements.is_empty() {
        text = apply_replacements(&text, &rules.replacements);
    }
    if !rules.snippets.is_empty() {
        text = apply_snippets(&text, &rules.snippets);
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
                full_stop: FullStop::Keep,
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
            full_stop: FullStop::Keep,
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
    fn phantom_words() {
        assert!(is_phantom("Yeah."));
        assert!(is_phantom(" you "));
        assert!(is_phantom("Thank you!"));
        assert!(!is_phantom("Yeah, let's do it."));
    }

    #[test]
    fn full_stop_modes() {
        let r = |mode| Rules {
            trailing_space: true,
            full_stop: mode,
            ..Rules::default()
        };
        let smart = r(FullStop::Smart);
        assert_eq!(clean("sounds good.", &smart), Some("Sounds good ".into()));
        assert_eq!(
            clean("I'll be late. Start without me.", &smart),
            Some("I'll be late. Start without me. ".into())
        );
        assert_eq!(
            clean("Are you coming?", &smart),
            Some("Are you coming? ".into())
        );
        assert_eq!(
            clean("Wait for it...", &smart),
            Some("Wait for it... ".into())
        );
        assert_eq!(
            clean("that is all full stop", &smart),
            Some("That is all. ".into())
        );
        assert_eq!(clean("ठीक है।", &smart), Some("ठीक है ".into()));
        let never = r(FullStop::Never);
        assert_eq!(
            clean("I'll be late. Start without me.", &never),
            Some("I'll be late. Start without me ".into())
        );
        let keep = r(FullStop::Keep);
        assert_eq!(clean("sounds good.", &keep), Some("Sounds good. ".into()));
    }

    #[test]
    fn trailing_space() {
        assert_eq!(clean("hi", &Rules::default()), Some("Hi ".into()));
    }

    #[test]
    fn unicode_is_kept() {
        assert_eq!(c("नमस्ते दुनिया"), Some("नमस्ते दुनिया".into()));
    }

    #[test]
    fn scratch_that() {
        assert!(is_scratch("Scratch that."));
        assert!(is_scratch(" delete that "));
        assert!(!is_scratch("Please scratch that idea."));
    }

    #[test]
    fn snippets() {
        let rules = Rules {
            snippets: vec![("my address".into(), "12 Park Street\nDelhi".into())],
            trailing_space: false,
            full_stop: FullStop::Keep,
            ..Rules::default()
        };
        assert_eq!(
            clean("My address.", &rules).as_deref(),
            Some("12 Park Street\nDelhi")
        );
        assert_eq!(
            clean("Send it to my address please.", &rules).as_deref(),
            Some("Send it to 12 Park Street\nDelhi please.")
        );
    }

    #[test]
    fn learns_small_fixes_only() {
        assert_eq!(
            learn("I use vani every day.", "I use Vaani every day."),
            Some(("vani".into(), "Vaani".into()))
        );
        assert_eq!(
            learn("Meet sack tomorrow", "Meet Saksham Malhotra tomorrow"),
            Some(("sack".into(), "Saksham Malhotra".into()))
        );
        assert_eq!(learn("Hello there.", "Hello there!"), None);
        assert_eq!(learn("one two three four five", "a b c d e"), None);
    }
}
