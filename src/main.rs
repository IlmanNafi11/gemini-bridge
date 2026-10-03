//! Gemini Bridge binary entry point.
//!
//! Wires production services from configuration and starts the HTTP server.

mod cli;

use clap::Parser;
use gemini_bridge_config::BridgeConfig;
use gemini_bridge_conversation_store::SqliteConversationStore;
use gemini_bridge_gallery::{DefaultGalleryService, GalleryService};
use gemini_bridge_health_admin::{
    DefaultHealthAdminService, DefaultMediaPurgeAdminService, HealthAdminService,
    MediaPurgeAdminService,
};
use gemini_bridge_http_server::{
    AppState, ServerConfig, ServerOptions, TokenBucketConfig, build_reloadable_gemini_adapter,
    build_tool_engine, start_server_with_options_and_shutdown,
};
use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
use gemini_bridge_image_gen::{DefaultImageGenService, ImageGenService};
use gemini_bridge_media_store::{LocalMediaStore, MediaStore};
use gemini_bridge_transport::{ReqwestTransport, TransportService};
use gemini_bridge_upload::{HttpPushUploadClient, UploadLimits, UploadService, UploadServiceImpl};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

use crate::cli::{AuthCommand, Cli, Command};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    init_tracing();
    let args = Cli::parse();

    let default_config = Path::new("bridge.toml");
    let config_path = args
        .config
        .as_deref()
        .or_else(|| default_config.exists().then_some(default_config));
    let config = match BridgeConfig::load_composed(config_path, &args.composition()) {
        Ok(cfg) => cfg,
        Err(error) => {
            tracing::error!(%error, "failed to load configuration");
            return std::process::ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(Command::Auth { command }) => match command {
            AuthCommand::Login => handle_login(&config).await,
        },
        Some(Command::Doctor) => handle_doctor(&config).await,
        None => match run_server(config).await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(%error, "server startup failed");
                std::process::ExitCode::FAILURE
            }
        },
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .try_init();
}

/// Wire every service from configuration and start the HTTP server.
async fn run_server(config: BridgeConfig) -> Result<(), String> {
    let identity =
        Arc::new(DefaultIdentityService::new(&config).map_err(|error| error.to_string())?);
    match identity.bootstrap().await {
        Ok(_) => tracing::info!("Gemini session bootstrap succeeded"),
        Err(error) => {
            tracing::error!(%error, "Gemini session bootstrap failed; readiness remains unhealthy")
        }
    }
    let identity_svc: Arc<dyn IdentityService> = identity.clone();

    let (adapter, reloader) =
        build_reloadable_gemini_adapter(Arc::new(config.clone()), identity.clone())
            .map_err(|error| error.to_string())?;

    let local_store = LocalMediaStore::new(&config.storage.data_dir)
        .with_default_ttl_days(config.storage.media_ttl_days);
    let store: Arc<dyn MediaStore> = Arc::new(local_store.clone());
    let transport: Arc<dyn TransportService> =
        Arc::new(ReqwestTransport::new(&config.transport).map_err(|error| error.to_string())?);
    let push_client = Arc::new(
        HttpPushUploadClient::new(identity_svc.clone(), transport)
            .map_err(|error| error.to_string())?,
    );
    let upload = Arc::new(UploadServiceImpl::new(
        local_store.clone(),
        push_client,
        UploadLimits::default(),
    ));
    let upload_service: Option<Arc<dyn UploadService>> = Some(upload.clone());
    let image_service: Option<Arc<dyn ImageGenService>> = Some(Arc::new(
        DefaultImageGenService::new(adapter.clone(), local_store, upload, "gemini-web-flash"),
    ));

    let db_path = config.storage.data_dir.join("bridge.sqlite");
    let conversation_store = SqliteConversationStore::open(&db_path)
        .map(|service| {
            Arc::new(service) as Arc<dyn gemini_bridge_conversation_store::ConversationStore>
        })
        .map(Some)
        .map_err(|error| error.to_string())?;

    let gallery_service: Option<Arc<dyn GalleryService>> =
        Some(Arc::new(DefaultGalleryService::new(store.clone())));
    let media_purge: Option<Arc<dyn MediaPurgeAdminService>> =
        Some(Arc::new(DefaultMediaPurgeAdminService::new(store)));
    let health_admin: Arc<dyn HealthAdminService> = Arc::new(
        DefaultHealthAdminService::new(Some(identity_svc.clone())).with_reloader(reloader),
    );

    let bind_addr = format!("{}:{}", config.server.bind_addr, config.server.port)
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid bind address: {error}"))?;
    let app_state = AppState {
        adapter,
        upload_service,
        image_service,
        video_service: None,
        health_admin,
        identity_service: Some(identity_svc),
        conversation_store,
        tool_engine: build_tool_engine(),
        gallery_service,
        media_purge,
    };
    let rate_limit = config.server.rate_limit.map(|limit| TokenBucketConfig {
        capacity: limit.capacity,
        refill_tokens: limit.refill_tokens,
        refill_interval: Duration::from_secs(limit.refill_interval_secs),
    });
    let server_config = ServerConfig {
        bind_addr,
        api_key: config.server.api_key,
        require_key_for_admin: true,
        cors_enabled: config.server.cors_enabled,
        rate_limit,
        metrics_enabled: config.server.metrics_enabled,
    };
    let options = ServerOptions {
        cors_origins: config.server.cors_origins,
        concurrency_limit: config.server.concurrency_limit,
        body_limit_bytes: config.server.body_limit_bytes,
    };

    start_server_with_options_and_shutdown(server_config, app_state, options, shutdown_signal())
        .await
        .map_err(|error| error.to_string())
}

/// Resolve when SIGINT (Ctrl-C) or SIGTERM arrives so the server can drain.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

async fn handle_login(config: &BridgeConfig) -> std::process::ExitCode {
    let raw_cookie = match read_secret_stdin() {
        Ok(cookie) => cookie.trim().to_owned(),
        Err(error) => {
            eprintln!("Failed to read Cookie header securely: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if raw_cookie.is_empty() {
        eprintln!("Error: Cookie string is empty.");
        return std::process::ExitCode::FAILURE;
    }

    let service = match DefaultIdentityService::new(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error initializing identity service: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if let Err(e) = service.import_credentials_validated(&raw_cookie).await {
        eprintln!("Failed to import credentials: {e}");
        return std::process::ExitCode::FAILURE;
    }

    println!("Credentials successfully imported and secured.");
    std::process::ExitCode::SUCCESS
}

fn read_secret_stdin() -> io::Result<String> {
    #[cfg(unix)]
    {
        use std::io::BufRead;
        use std::process::{Command, Stdio};

        let saved = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !saved.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "standard input is not a terminal",
            ));
        }
        let saved = String::from_utf8_lossy(&saved.stdout).trim().to_owned();
        let hidden = Command::new("stty")
            .arg("-echo")
            .stdin(Stdio::inherit())
            .status()?;
        if !hidden.success() {
            let _ = Command::new("stty")
                .arg(&saved)
                .stdin(Stdio::inherit())
                .status();
            return Err(io::Error::other("could not disable terminal echo"));
        }
        eprint!("Paste the full Gemini Cookie header (input is hidden): ");
        if let Err(error) = io::stderr().flush() {
            let _ = Command::new("stty")
                .arg(&saved)
                .stdin(Stdio::inherit())
                .status();
            return Err(error);
        }

        let mut line = String::new();
        let read_result = io::stdin().lock().read_line(&mut line);
        let restored = Command::new("stty")
            .arg(saved)
            .stdin(Stdio::inherit())
            .status();
        eprintln!();

        match (read_result, restored) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(status)) if !status.success() => {
                Err(io::Error::other("could not restore terminal settings"))
            }
            (Ok(_), Ok(_)) => Ok(line),
        }
    }
    #[cfg(not(unix))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "hidden Cookie input is only supported on Unix terminals",
        ))
    }
}

async fn handle_doctor(config: &BridgeConfig) -> std::process::ExitCode {
    println!("Running gemini-bridge doctor...");
    println!("Data directory: {}", config.storage.data_dir.display());

    let service = match DefaultIdentityService::new(config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize identity service: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let snapshot = service.snapshot().await;
    match snapshot.status {
        gemini_bridge_identity::SessionStatus::Unconfigured => {
            println!("Session status: Unconfigured");
            println!("No credentials found. Run 'gemini-bridge auth login' to import credentials.");
            std::process::ExitCode::SUCCESS
        }
        _ => {
            println!("Probing Gemini Web (/app)...");
            match service.bootstrap().await {
                Ok(_bootstrap) => {
                    println!("Session status: Valid");
                    println!("Tokens: bl, SNlM0e, f.sid — present");
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Probe failed: {e}");
                    let snap = service.snapshot().await;
                    eprintln!("Session status: {:?}", snap.status);
                    std::process::ExitCode::FAILURE
                }
            }
        }
    }
}
