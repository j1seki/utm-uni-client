//! `utm-web` entry point.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use utm_web::cert;
use utm_web::config;
use utm_web::discovery::Discovery;
use utm_web::server::{router, AppState};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        error!("{error}");
        eprintln!("utm-web: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = config::parse_cli()?;
    if cli.help {
        config::print_help();
        return Ok(());
    }
    if cli.version {
        println!("utm-web {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    init_logging();

    let (app_config, profiles) = config::load(&cli)?;

    if cli.generate_cert {
        let identity = if app_config.cert_path.exists() {
            cert::load_identity(&app_config.cert_path, &app_config.cert_password)?
        } else {
            cert::generate_identity(&app_config.cert_path)?
        };
        println!("client certificate: {}", app_config.cert_path.display());
        println!("client fingerprint: {}", identity.fingerprint_string());
        return Ok(());
    }

    let identity = Arc::new(config::ensure_client_certificate(&app_config)?);
    info!(
        "client certificate {} (fingerprint {})",
        app_config.cert_path.display(),
        identity.fingerprint_string()
    );

    let config_path = app_config.config_path.clone();
    let listen = app_config.listen;
    let profiles = Arc::new(profiles);
    let discovery = Discovery::start();
    let state = AppState::new(app_config, identity, profiles, discovery.clone());

    if !listen.ip().is_loopback() {
        warn!(
            "listening on a non-loopback address ({listen}); the web UI has no authentication. \
             Put it behind a reverse proxy with TLS and access control."
        );
    }

    let app = router(Arc::clone(&state));

    let listener = TcpListener::bind(listen)
        .await
        .map_err(|e| utm_web::Error::Config(format!("cannot listen on {listen}: {e}")))?;
    let local = listener.local_addr().unwrap_or(listen);
    let url = match local.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => format!("http://localhost:{}", local.port()),
        IpAddr::V6(ip) if ip.is_unspecified() => format!("http://localhost:{}", local.port()),
        ip => format!("http://{ip}:{}", local.port()),
    };
    info!("config file: {}", config_path.display());
    info!("utm-web {} listening on {url}", env!("CARGO_PKG_VERSION"));

    let reaper_state = Arc::clone(&state);
    let reaper = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            reaper_state.reap_displays().await;
        }
    });

    let shutdown = shutdown_signal();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;

    reaper.abort();
    state.disconnect_all().await;
    discovery.shutdown();

    result?;
    info!("shut down");
    Ok(())
}

fn init_logging() {
    let filter =
        EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("utm_web=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received Ctrl-C, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
