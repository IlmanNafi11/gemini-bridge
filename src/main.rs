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
    AppState, ServerConfig, build_reloadable_gemini_adapter, build_tool_engine, start_server,
};
use gemini_bridge_identity::{DefaultIdentityService, IdentityService};
use gemini_bridge_image_gen::{DefaultImageGenService, ImageGenService};
use gemini_bridge_media_store::{LocalMediaStore, MediaStore};
use gemini_bridge_transport::{ReqwestTransport, TransportService};
use gemini_bridge_upload::{HttpPushUploadClient, UploadLimits, UploadService, UploadServiceImpl};
use std::io::{self, BufRead};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use crate::cli::{AuthCommand, Cli, Command};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    init_tracing();
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
        Err(error) => {
            tracing::error!(%error, "failed to load configuration");
            return std::process::ExitCode::FAILURE;
        }
    };

    match args.command {
        Some(Command::Auth { command }) => match command {
            AuthCommand::Login { cookie } => handle_login(&config, cookie).await,
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

    let (adapter, reloader) =
        build_reloadable_gemini_adapter(Arc::new(config.clone()), identity.clone())
            .map_err(|error| error.to_string())?;

    let local_store = LocalMediaStore::new(&config.storage.data_dir);
    let store: Arc<dyn MediaStore> = Arc::new(local_store.clone());

    let transport: Arc<dyn TransportService> =
        Arc::new(ReqwestTransport::new(&config.transport).map_err(|error| error.to_string())?);
    let push_client = Arc::new(
        HttpPushUploadClient::new(identity.clone() as Arc<dyn IdentityService>, transport)
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
        DefaultHealthAdminService::new(Some(identity as Arc<dyn IdentityService>))
            .with_reloader(reloader),
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
        conversation_store,
        tool_engine: build_tool_engine(),
        gallery_service,
        media_purge,
    };
    let server_config = ServerConfig {
        bind_addr,
        api_key: config.server.api_key,
        require_key_for_admin: true,
        cors_enabled: config.server.cors_enabled,
        rate_limit: None,
        metrics_enabled: config.server.metrics_enabled,
    };

    start_server(server_config, app_state)
        .await
        .map_err(|error| error.to_string())
}

async fn handle_login(config: &BridgeConfig, cookie: Option<String>) -> std::process::ExitCode {
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
                return std::process::ExitCode::FAILURE;
            }
            line.trim().to_string()
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

    if let Err(e) = service.import_credentials(&raw_cookie).await {
        eprintln!("Failed to import credentials: {e}");
        return std::process::ExitCode::FAILURE;
    }

    println!("Credentials successfully imported and secured.");
    std::process::ExitCode::SUCCESS
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
