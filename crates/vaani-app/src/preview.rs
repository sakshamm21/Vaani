//! Live preview: while the speaker talks, the local model re-reads the last few seconds of
//! audio and the words are shown above the pill. Each read overlaps the one before, and
//! [`merge`] stitches them into one running caption.
//!
//! This is only for show. The inserted text always comes from a full pass over the whole
//! recording once the key is released.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use vaani_models::ModelKind;

/// How much audio each preview reads (16 kHz samples).
pub const WINDOW: usize = 16_000 * 4;
/// Wait at least this long between the starts of two previews, and a few times as long as
/// the last one took, so the model is mostly free when the key is released.
const MIN_GAP: Duration = Duration::from_millis(400);
/// A cloud-engine user's preview model is unloaded after this long without dictating.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

/// Models fast enough to re-read a few seconds several times a second on a laptop CPU.
/// Whisper pads every input to 30 seconds, so each preview would cost a full decode.
pub fn supported(kind: ModelKind) -> bool {
    !matches!(kind, ModelKind::Whisper)
}

#[derive(Default)]
pub struct Preview {
    /// The caption so far.
    pub text: String,
    /// The read in flight: whether its window was cut from longer audio, and its result.
    pending: Option<(bool, Receiver<Option<String>>)>,
    started: Option<Instant>,
    pub last_took: Duration,
}

impl Preview {
    /// Is it time to start another read? `shares_model`: the final pass uses the same model.
    pub fn due(&self, shares_model: bool) -> bool {
        let gap = self.last_took * if shares_model { 3 } else { 2 };
        self.pending.is_none() && self.started.is_none_or(|t| t.elapsed() >= MIN_GAP.max(gap))
    }

    pub fn started(&mut self, cut: bool, rx: Receiver<Option<String>>) {
        self.started = Some(Instant::now());
        self.pending = Some((cut, rx));
    }

    /// Take a finished read into the caption. Returns true when the caption changed.
    pub fn collect(&mut self) -> bool {
        let Some((cut, rx)) = &self.pending else {
            return false;
        };
        let got = match rx.try_recv() {
            Ok(got) => got,
            Err(crossbeam_channel::TryRecvError::Empty) => return false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => None,
        };
        let cut = *cut;
        self.pending = None;
        if let Some(t) = self.started {
            self.last_took = t.elapsed();
        }
        let Some(new) = got else { return false };
        let merged = merge(&self.text, &new, cut);
        if merged == self.text {
            return false;
        }
        self.text = merged;
        true
    }
}

fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Fold a new read into the caption. `cut` means the read started part-way through the
/// recording, so its first word may be half a word and is dropped.
pub fn merge(old: &str, new: &str, cut: bool) -> String {
    /// Longest caption kept, in words; the pill only shows the end of it.
    const KEEP: usize = 60;
    let mut new: Vec<&str> = new.split_whitespace().collect();
    if !cut {
        // The read covered the whole recording, so it is the best caption there is.
        return if new.is_empty() {
            old.into()
        } else {
            new.join(" ")
        };
    }
    if !new.is_empty() {
        new.remove(0);
    }
    if new.is_empty() {
        // Silence (or one word): keep what is shown.
        return old.into();
    }
    let old: Vec<&str> = old.split_whitespace().collect();
    let (on, nn): (Vec<String>, Vec<String>) = (
        old.iter().map(|w| norm(w)).collect(),
        new.iter().map(|w| norm(w)).collect(),
    );
    // Line the new read up with the end of the caption: the longest run of matching words
    // between the caption's last words and the new read, which may start with a few words
    // misheard at the edge of its window.
    let mut best = (0usize, 0usize, 0usize); // (run, caption position, new position)
    for j in 0..nn.len().min(4) {
        for i in old.len().saturating_sub(24)..old.len() {
            let run = on[i..]
                .iter()
                .zip(&nn[j..])
                .take_while(|(a, b)| a == b && !a.is_empty())
                .count();
            if run > best.0 {
                best = (run, i, j);
            }
        }
    }
    let (run, i, j) = best;
    let words: Vec<&str> = if run >= 2 || (run == 1 && j == 0 && nn.len() == 1) {
        // The newer read wins for the words they share: it heard more of the sentence.
        old[..i].iter().chain(&new[j..]).copied().collect()
    } else {
        // They don't line up: show the new read alone rather than risk repeating words.
        new
    };
    words[words.len().saturating_sub(KEEP)..].join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_recording_replaces() {
        assert_eq!(merge("hello", "Hello there.", false), "Hello there.");
        assert_eq!(merge("hello there", "", false), "hello there");
    }

    #[test]
    fn overlapping_reads_join() {
        let a = "So I think we should";
        let b = "nk we should ship it on Friday";
        assert_eq!(merge(a, b, true), "So I think we should ship it on Friday");
    }

    #[test]
    fn newer_read_corrects_the_overlap() {
        let a = "the meeting is at for";
        let b = "ing is at four thirty today.";
        assert_eq!(merge(a, b, true), "the meeting is at four thirty today.");
    }

    #[test]
    fn punctuation_and_case_do_not_block_alignment() {
        let a = "Thanks, John. See you";
        let b = "ks john see you tomorrow";
        assert_eq!(merge(a, b, true), "Thanks, john see you tomorrow");
    }

    #[test]
    fn silence_keeps_the_caption() {
        assert_eq!(merge("one two three", "", true), "one two three");
        assert_eq!(merge("one two three", "uh", true), "one two three");
    }

    #[test]
    fn misheard_start_of_a_read_is_skipped() {
        let a = "show the while I keep talking and the";
        let b = "x the uh the while I keep talking and the caption.";
        assert_eq!(
            merge(a, b, true),
            "show the while I keep talking and the caption."
        );
    }

    #[test]
    fn no_overlap_shows_the_new_read() {
        assert_eq!(merge("first part", "x second part", true), "second part");
    }
}
