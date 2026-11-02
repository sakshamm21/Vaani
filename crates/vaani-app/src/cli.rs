use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "vaani",
    version,
    about = "Hold a key, speak, release: text appears where you're typing."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Start the app in the background (the default when no command is given).
    Run,
    /// Start dictating, or finish if already dictating.
    Toggle,
    /// Start dictating.
    Start,
    /// Finish dictating and insert the text.
    Stop,
    /// Throw away the current dictation.
    Cancel,
    /// Print what the running app is doing: idle, recording or processing.
    Status,
    /// Stop the running app.
    Quit,
    /// Check this computer's setup and print a report to paste into bug reports.
    Doctor {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}
