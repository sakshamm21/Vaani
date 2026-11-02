//! The dictation state machine.
//!
//! Every input (hotkey, CLI, D-Bus, tray) becomes a [`Command`]; the machine decides what
//! happens next. Later milestones attach audio, transcription and insertion to the transitions.

use std::fmt;

/// Where a dictation is right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for the hotkey.
    Idle,
    /// The microphone is open and audio is being captured.
    Recording,
    /// Audio is captured; transcription, cleanup and insertion are running.
    Processing,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Processing => "processing",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something that asks the machine to change state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Toggle,
    /// Throw the current dictation away (Esc).
    Cancel,
    /// The pipeline finished (text inserted, or failed and saved to history).
    Finished,
}

/// What the caller should do as a result of a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    StartCapture,
    StopCaptureAndProcess,
    DiscardCapture,
    AbortProcessing,
}

#[derive(Debug)]
pub struct Machine {
    state: State,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self { state: State::Idle }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Apply a command and return the side effect the caller must perform.
    pub fn handle(&mut self, cmd: Command) -> Effect {
        use Command as C;
        use State as S;
        let (next, effect) = match (self.state, cmd) {
            (S::Idle, C::Start | C::Toggle) => (S::Recording, Effect::StartCapture),
            (S::Recording, C::Stop | C::Toggle) => (S::Processing, Effect::StopCaptureAndProcess),
            (S::Recording, C::Cancel) => (S::Idle, Effect::DiscardCapture),
            (S::Processing, C::Cancel) => (S::Idle, Effect::AbortProcessing),
            (S::Processing, C::Finished) => (S::Idle, Effect::None),
            // Everything else is ignored: e.g. Start while recording, Stop while idle.
            (s, _) => (s, Effect::None),
        };
        if next != self.state {
            tracing::debug!(from = %self.state, to = %next, ?cmd, "state change");
        }
        self.state = next;
        effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_cycle() {
        let mut m = Machine::new();
        assert_eq!(m.handle(Command::Start), Effect::StartCapture);
        assert_eq!(m.state(), State::Recording);
        assert_eq!(m.handle(Command::Stop), Effect::StopCaptureAndProcess);
        assert_eq!(m.state(), State::Processing);
        assert_eq!(m.handle(Command::Finished), Effect::None);
        assert_eq!(m.state(), State::Idle);
    }

    #[test]
    fn toggle_twice_records_then_processes() {
        let mut m = Machine::new();
        m.handle(Command::Toggle);
        assert_eq!(m.state(), State::Recording);
        m.handle(Command::Toggle);
        assert_eq!(m.state(), State::Processing);
        // Toggle during processing does not start a second dictation.
        assert_eq!(m.handle(Command::Toggle), Effect::None);
        assert_eq!(m.state(), State::Processing);
    }

    #[test]
    fn cancel_from_each_state() {
        let mut m = Machine::new();
        assert_eq!(m.handle(Command::Cancel), Effect::None);
        m.handle(Command::Start);
        assert_eq!(m.handle(Command::Cancel), Effect::DiscardCapture);
        assert_eq!(m.state(), State::Idle);
        m.handle(Command::Start);
        m.handle(Command::Stop);
        assert_eq!(m.handle(Command::Cancel), Effect::AbortProcessing);
        assert_eq!(m.state(), State::Idle);
    }

    #[test]
    fn stray_commands_are_ignored() {
        let mut m = Machine::new();
        assert_eq!(m.handle(Command::Stop), Effect::None);
        assert_eq!(m.handle(Command::Finished), Effect::None);
        m.handle(Command::Start);
        assert_eq!(m.handle(Command::Start), Effect::None);
        assert_eq!(m.state(), State::Recording);
    }
}
