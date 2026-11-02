//! Start, stop and cancel cues.

use vaani_core::config::Config;

#[derive(Clone, Copy)]
pub enum Cue {
    Start,
    Stop,
    Cancel,
}

pub fn play(config: &Config, cue: Cue) {
    if !config.general.sounds {
        return;
    }
    let tones: &'static [(f32, u32)] = match cue {
        Cue::Start => &[(660.0, 55), (880.0, 70)],
        Cue::Stop => &[(880.0, 55), (660.0, 70)],
        Cue::Cancel => &[(392.0, 90)],
    };
    vaani_audio::cue::play_tones(tones, 0.12);
}
