use std::path::PathBuf;

use clap::{Parser, Subcommand};
use gemini_bridge_config::Composition;

/// Gemini Web to OpenAI-compatible bridge.
#[derive(Debug, Parser)]
#[command(name = "gemini-bridge", version, about)]
pub struct Cli {
    /// Path to bridge configuration. If omitted, an existing `bridge.toml` is loaded;
    /// otherwise built-in defaults are used.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Named configuration bundle to apply. Repeat to compose in order.
    #[arg(long, global = true, action = clap::ArgAction::Append)]
    pub bundle: Vec<String>,

    /// Named configuration profile to apply. Repeat to compose in order.
    #[arg(long, global = true, action = clap::ArgAction::Append)]
    pub profile: Vec<String>,

    /// Named configuration patch to apply last. Repeat to compose in order.
    #[arg(long, global = true, action = clap::ArgAction::Append)]
    pub patch: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn composition(&self) -> Composition {
        let with_bundles = self
            .bundle
            .iter()
            .fold(Composition::new(), |composition, name| {
                composition.with_bundle(name)
            });
        let with_profiles = self.profile.iter().fold(with_bundles, |composition, name| {
            composition.with_profile(name)
        });
        self.patch.iter().fold(with_profiles, |composition, name| {
            composition.with_patch(name)
        })
    }
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
    /// Import a Gemini Cookie header from a no-echo standard-input prompt.
    Login,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;
    use gemini_bridge_config::Composition;

    #[test]
    fn accepts_repeated_bundle_profile_and_patch_selectors() {
        let cli = Cli::try_parse_from([
            "gemini-bridge",
            "--bundle",
            "edge",
            "--bundle",
            "public",
            "--profile",
            "prod",
            "--patch",
            "operator",
        ])
        .unwrap();

        assert_eq!(
            cli.composition(),
            Composition::new()
                .with_bundle("edge")
                .with_bundle("public")
                .with_profile("prod")
                .with_patch("operator")
        );
    }
    #[test]
    fn accepts_selectors_without_subcommand() {
        let cli = Cli::try_parse_from(["gemini-bridge", "--profile", "prod"]).unwrap();
        assert_eq!(cli.composition(), Composition::new().with_profile("prod"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn login_rejects_cookie_command_line_argument() {
        assert!(
            Cli::try_parse_from(["gemini-bridge", "auth", "login", "--cookie", "secret",]).is_err()
        );
    }
}
