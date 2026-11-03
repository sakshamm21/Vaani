//! Hindi in Latin letters ("Hinglish" spelling), for people who type Hindi that way.
//! Speech models write Hindi in Devanagari; this turns it into the plain spelling people use
//! in chats ("aap kaise ho"), without diacritics. Other text passes through unchanged.

fn consonant(c: char) -> Option<&'static str> {
    Some(match c {
        'क' => "k",
        'ख' => "kh",
        'ग' => "g",
        'घ' => "gh",
        'ङ' => "n",
        'च' => "ch",
        'छ' => "chh",
        'ज' => "j",
        'झ' => "jh",
        'ञ' => "n",
        'ट' => "t",
        'ठ' => "th",
        'ड' => "d",
        'ढ' => "dh",
        'ण' => "n",
        'त' => "t",
        'थ' => "th",
        'द' => "d",
        'ध' => "dh",
        'न' => "n",
        'प' => "p",
        'फ' => "ph",
        'ब' => "b",
        'भ' => "bh",
        'म' => "m",
        'य' => "y",
        'र' => "r",
        'ल' => "l",
        'व' => "v",
        'श' | 'ष' => "sh",
        'स' => "s",
        'ह' => "h",
        // Precomposed nukta letters.
        '\u{958}' => "q",
        '\u{959}' => "kh",
        '\u{95a}' => "g",
        '\u{95b}' => "z",
        '\u{95c}' => "r",
        '\u{95d}' => "rh",
        '\u{95e}' => "f",
        '\u{95f}' => "y",
        _ => return None,
    })
}

/// The spelling of a consonant followed by a nukta (़).
fn with_nukta(c: char) -> Option<&'static str> {
    Some(match c {
        'क' => "q",
        'ख' => "kh",
        'ग' => "g",
        'ज' => "z",
        'ड' => "r",
        'ढ' => "rh",
        'फ' => "f",
        _ => return None,
    })
}

fn vowel(c: char) -> Option<&'static str> {
    Some(match c {
        'अ' => "a",
        'आ' => "aa",
        'इ' => "i",
        'ई' => "ee",
        'उ' => "u",
        'ऊ' => "oo",
        'ऋ' => "ri",
        'ए' => "e",
        'ऐ' => "ai",
        'ओ' => "o",
        'औ' => "au",
        'ऑ' => "o",
        _ => return None,
    })
}

fn matra(c: char) -> Option<&'static str> {
    Some(match c {
        'ा' => "aa",
        'ि' => "i",
        'ी' => "ee",
        'ु' => "u",
        'ू' => "oo",
        'ृ' => "ri",
        'े' => "e",
        'ै' => "ai",
        'ो' => "o",
        'ौ' => "au",
        'ॉ' => "o",
        _ => return None,
    })
}

const VIRAMA: char = '्';
const NUKTA: char = '़';

/// One letter of a word: a consonant (with its vowel sign, if any) or a bare vowel.
#[derive(Debug)]
enum Letter {
    /// Consonant spelling, and its vowel: Some(matra), None for the inherent "a", or
    /// Some("") after a virama.
    C(&'static str, Option<&'static str>),
    V(&'static str),
    /// Anusvara, chandrabindu, visarga or anything else written as is.
    Other(String),
}

fn letters(word: &str) -> Vec<Letter> {
    let mut out = Vec::new();
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(mut cons) = consonant(c) {
            if chars.peek() == Some(&NUKTA) {
                chars.next();
                cons = with_nukta(c).unwrap_or(cons);
            }
            let v = match chars.peek().copied() {
                Some(VIRAMA) => {
                    chars.next();
                    Some("")
                }
                Some(m) => matra(m).inspect(|_| {
                    chars.next();
                }),
                None => None,
            };
            out.push(Letter::C(cons, v));
        } else if let Some(v) = vowel(c) {
            out.push(Letter::V(v));
        } else {
            let s = match c {
                'ं' | 'ँ' => "n".to_string(),
                'ः' => "h".to_string(),
                '।' | '॥' => ".".to_string(),
                '०'..='९' => char::from(b'0' + (c as u32 - '०' as u32) as u8).to_string(),
                NUKTA | VIRAMA => String::new(),
                c => c.to_string(),
            };
            out.push(Letter::Other(s));
        }
    }
    out
}

fn romanize_word(word: &str) -> String {
    let ls = letters(word);
    // Which letters end up with a vowel sound. Hindi drops the inherent "a" at the end of a
    // word ("kaam", not "kaama") and between a vowel and a consonant that has its own vowel
    // ("samajhna"). Deciding right to left lets each drop see the ones after it.
    let mut voiced: Vec<bool> = ls
        .iter()
        .map(|l| {
            matches!(l, Letter::V(_) | Letter::C(_, None))
                || matches!(l, Letter::C(_, Some(v)) if !v.is_empty())
        })
        .collect();
    let sounds = |l: &Letter| matches!(l, Letter::C(..) | Letter::V(_));
    let last = ls.iter().rposition(sounds);
    let mut drop = vec![false; ls.len()];
    for i in (0..ls.len()).rev() {
        if !matches!(ls[i], Letter::C(_, None)) {
            continue;
        }
        let at_end = Some(i) == last && i > 0;
        let between = i > 0
            && voiced[i - 1]
            && matches!(ls.get(i + 1), Some(Letter::C(..)))
            && voiced.get(i + 1) == Some(&true);
        if at_end || between {
            drop[i] = true;
            voiced[i] = false;
        }
    }
    let mut out = String::new();
    for (i, l) in ls.iter().enumerate() {
        match l {
            Letter::V(v) => out.push_str(v),
            Letter::Other(s) => out.push_str(s),
            Letter::C(c, Some(v)) => {
                out.push_str(c);
                // A long "aa" at the end of a word is written "a" ("mera", "kya").
                out.push_str(if *v == "aa" && Some(i) == last {
                    "a"
                } else {
                    v
                });
            }
            Letter::C(c, None) => {
                out.push_str(c);
                if !drop[i] {
                    out.push('a');
                }
            }
        }
    }
    out
}

/// Whether `text` contains Devanagari.
pub fn has_devanagari(text: &str) -> bool {
    text.chars().any(|c| ('\u{900}'..='\u{97f}').contains(&c))
}

/// Write Devanagari words in Latin letters; everything else is kept.
pub fn romanize(text: &str) -> String {
    if !has_devanagari(text) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for c in text.chars() {
        if ('\u{900}'..='\u{97f}').contains(&c) {
            word.push(c);
        } else {
            if !word.is_empty() {
                out.push_str(&romanize_word(&word));
                word.clear();
            }
            out.push(c);
        }
    }
    if !word.is_empty() {
        out.push_str(&romanize_word(&word));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_words() {
        for (hi, en) in [
            ("आप कैसे हो", "aap kaise ho"),
            ("मैं ठीक हूँ", "main theek hoon"),
            ("काम", "kaam"),
            ("नमस्ते", "namaste"),
            ("समझना", "samajhna"),
            ("क्या", "kya"),
            ("ज़रूर", "zaroor"),
            ("घर", "ghar"),
            ("मेरा नाम राम है।", "mera naam raam hai."),
            ("कल 5 बजे meeting है", "kal 5 baje meeting hai"),
        ] {
            assert_eq!(romanize(hi), en, "{hi}");
        }
    }

    #[test]
    fn other_text_untouched() {
        assert_eq!(romanize("Hello, world!"), "Hello, world!");
        assert!(!has_devanagari("abc"));
    }
}
