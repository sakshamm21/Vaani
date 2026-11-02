//! Session probe, active window, MPRIS and notifications.
//!
//! Only the session probe exists so far. It reads the environment and a few files to work out
//! what kind of desktop we are on, so the hotkey and insertion backends can be chosen from what
//! the session actually supports ("detect, don't assume").

pub mod probe;

pub use probe::{Desktop, SessionInfo, SessionType, probe};
