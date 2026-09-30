use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Gemini Web to OpenAI-compatible bridge.
#[derive(Debug, Parser)]
#[command(name = "gemini-bridge", version, about)]
pub struct Cli {
    /// Path to bridge configuration.
    #[arg(long, global = true, default_value = "bridge.toml")]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage local Gemini authentication.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Probe configured credentials and Gemini connectivity.
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Import a Gemini Cookie header into secure local storage.
    Login {
        /// Complete Cookie header. Omit to read it securely from standard input.
        #[arg(long, env = "GEMINI_COOKIE")]
        cookie: Option<String>,
    },
}
