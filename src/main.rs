mod cli;

use std::io::{self, BufRead};
use std::process::ExitCode;

use clap::Parser;
use gemini_bridge_config::BridgeConfig;
use gemini_bridge_identity::{DefaultIdentityService, IdentityService, SessionStatus};

use crate::cli::{AuthCommand, Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let args = Cli::parse();

    let config = match BridgeConfig::load_with_overrides(
        if args.config.exists() {
            Some(&args.config)
        } else {
            None
        },
        None,
    ) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error loading configuration: {e}");
            return ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(Command::Auth { command }) => match command {
            AuthCommand::Login { cookie } => handle_login(&config, cookie).await,
        },
        Some(Command::Doctor) => handle_doctor(&config).await,
        None => {
            println!(
                "Starting gemini-bridge server on {}:{}...",
                config.server.bind_addr, config.server.port
            );
            println!("(Server implementation scheduled for next milestone)");
            ExitCode::SUCCESS
        }
    }
}

async fn handle_login(config: &BridgeConfig, cookie: Option<String>) -> ExitCode {
    let raw_cookie = match cookie {
        Some(c) => c,
        None => {
            println!(
                "Please paste the full Gemini 'Cookie:' header (containing __Secure-1PSID, __Secure-1PSIDTS, SAPISID):"
            );
            let stdin = io::stdin();
            let mut line = String::new();
            if let Err(e) = stdin.lock().read_line(&mut line) {
                eprintln!("Failed to read input: {e}");
                return ExitCode::FAILURE;
            }
            line.trim().to_string()
        }
    };

    if raw_cookie.is_empty() {
        eprintln!("Error: Cookie string is empty.");
        return ExitCode::FAILURE;
    }

    let service = match DefaultIdentityService::new(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error initializing identity service: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = service.import_credentials(&raw_cookie).await {
        eprintln!("Failed to import credentials: {e}");
        return ExitCode::FAILURE;
    }

    println!("Credentials successfully imported and secured.");
    ExitCode::SUCCESS
}

async fn handle_doctor(config: &BridgeConfig) -> ExitCode {
    println!("Running gemini-bridge doctor...");
    println!("Data directory: {}", config.storage.data_dir.display());

    let service = match DefaultIdentityService::new(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize identity service: {e}");
            return ExitCode::FAILURE;
        }
    };

    let snapshot = service.snapshot().await;
    match snapshot.status {
        SessionStatus::Unconfigured => {
            println!("Session status: Unconfigured");
            println!("No credentials found. Run 'gemini-bridge auth login' to import credentials.");
            ExitCode::SUCCESS
        }
        _ => {
            println!("Probing Gemini Web (/app)...");
            match service.bootstrap().await {
                Ok(_bootstrap) => {
                    println!("Session status: Valid");
                    println!("Tokens: bl, SNlM0e, f.sid — present");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Probe failed: {e}");
                    let snap = service.snapshot().await;
                    eprintln!("Session status: {:?}", snap.status);
                    ExitCode::FAILURE
                }
            }
        }
    }
}
