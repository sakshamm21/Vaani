//! State machine, pipeline, events and config schema.

pub mod config;
pub mod paths;
pub mod state;

/// The app's display name. Change it here (and in `paths`) if the project is renamed.
pub const APP_NAME: &str = "Vaani";

/// The D-Bus well-known name and object path the running instance owns.
pub const DBUS_NAME: &str = "io.github.sakshamm21.Vaani";
pub const DBUS_PATH: &str = "/io/github/sakshamm21/Vaani";
