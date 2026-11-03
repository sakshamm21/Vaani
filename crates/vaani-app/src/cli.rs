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
    /// Print what the running app is doing.
    Status,
    /// Paste the last dictation again.
    PasteLast,
    /// Command mode: start, or finish, saying how to change the selected text.
    Command,
    /// Open the settings window.
    Settings,
    /// Stop the running app.
    Quit,
    /// Manage local speech models.
    Models {
        #[command(subcommand)]
        action: Option<ModelsCmd>,
    },
    /// Show recent dictations ("!" marks ones that were not pasted).
    History {
        /// How many to show.
        #[arg(default_value_t = 20)]
        count: usize,
    },
    /// Run the AI clean-up on some text with your current settings, to try them out.
    Cleanup {
        /// The text to clean up.
        text: Option<String>,
        /// Clean every line of this file and report each result (for testing prompts).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// List the models your AI provider offers instead.
        #[arg(long)]
        models: bool,
    },
    /// Check this computer's setup and print a report to paste into bug reports.
    Doctor {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// List available models.
    List,
    /// Download a model.
    Download {
        id: String,
        /// Also switch Vaani to this model.
        #[arg(long)]
        r#use: bool,
    },
    /// Delete a downloaded model.
    Remove { id: String },
}
