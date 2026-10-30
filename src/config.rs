use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::cert::{generate_identity, DEFAULT_CERT_PASSWORD};
use crate::error::{Error, Result};
use crate::profiles::ProfileStore;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub listen: SocketAddr,
    pub config_path: PathBuf,
    pub cert_path: PathBuf,
    pub cert_password: String,
    pub allow_untrusted: bool,
    pub max_sessions: usize,
    pub session_idle_timeout: Duration,
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub listen: Option<String>,
    pub client_certificate: Option<String>,
    pub certificate_password: Option<String>,
    pub allow_untrusted: Option<bool>,
    pub max_sessions: Option<usize>,
    pub session_idle_timeout_secs: Option<u64>,
    pub public_url: Option<String>,
    pub profiles: Vec<crate::profiles::Profile>,
}

pub fn default_config_dir() -> PathBuf {
    if let Ok(dir) = env::var("UTM_WEB_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = env::var("APPDATA") {
            return PathBuf::from(appdata).join("utm-web");
        }
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("utm-web");
        }
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".config").join("utm-web");
    }
    PathBuf::from(".utm-web")
}

pub fn default_config_path() -> PathBuf {
    default_config_dir().join("config.json")
}

fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn env_bool(name: &str) -> Option<bool> {
    env::var(name)
        .ok()
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
}

pub struct Cli {
    pub listen: Option<String>,
    pub config: Option<PathBuf>,
    pub cert: Option<PathBuf>,
    pub cert_password: Option<String>,
    pub generate_cert: bool,
    pub help: bool,
    pub version: bool,
}

pub fn parse_cli() -> Result<Cli> {
    let mut cli = Cli {
        listen: None,
        config: None,
        cert: None,
        cert_password: None,
        generate_cert: false,
        help: false,
        version: false,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
            "--generate-cert" => cli.generate_cert = true,
            "--listen" => {
                cli.listen = Some(
                    args.next()
                        .ok_or_else(|| Error::Config("--listen needs a value".into()))?,
                )
            }
            "--config" => {
                cli.config =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        Error::Config("--config needs a value".into())
                    })?))
            }
            "--cert" => {
                cli.cert =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        Error::Config("--cert needs a value".into())
                    })?))
            }
            "--cert-password" => {
                cli.cert_password = Some(
                    args.next()
                        .ok_or_else(|| Error::Config("--cert-password needs a value".into()))?,
                )
            }
            other if other.starts_with("--listen=") => {
                cli.listen = Some(other["--listen=".len()..].to_owned())
            }
            other if other.starts_with("--config=") => {
                cli.config = Some(PathBuf::from(&other["--config=".len()..]))
            }
            other if other.starts_with("--cert=") => {
                cli.cert = Some(PathBuf::from(&other["--cert=".len()..]))
            }
            other if other.starts_with("--cert-password=") => {
                cli.cert_password = Some(other["--cert-password=".len()..].to_owned())
            }
            other => {
                return Err(Error::Config(format!(
                    "unknown argument {other:?}; try --help"
                )))
            }
        }
    }
    Ok(cli)
}

pub fn load(cli: &Cli) -> Result<(AppConfig, ProfileStore)> {
    let config_path = cli
        .config
        .clone()
        .or_else(|| env::var("UTM_WEB_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(default_config_path);

    let file = if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)
            .map_err(|e| Error::Config(format!("cannot read config {config_path:?}: {e}")))?;
        serde_json::from_str::<ConfigFile>(&text)
            .map_err(|e| Error::Config(format!("invalid config {config_path:?}: {e}")))?
    } else {
        ConfigFile::default()
    };

    let listen_text = cli
        .listen
        .clone()
        .or_else(|| env::var("UTM_WEB_LISTEN").ok())
        .or_else(|| file.listen.clone())
        .unwrap_or_else(|| "127.0.0.1:8080".to_owned());
    let listen = listen_text
        .parse::<SocketAddr>()
        .map_err(|e| Error::Config(format!("invalid listen address {listen_text:?}: {e}")))?;

    let cert_path = cli
        .cert
        .clone()
        .or_else(|| env::var("UTM_WEB_CERT").ok().map(PathBuf::from))
        .or_else(|| file.client_certificate.as_deref().map(expand_path))
        .unwrap_or_else(|| default_config_dir().join("client.pem"));

    let cert_password = cli
        .cert_password
        .clone()
        .or_else(|| env::var("UTM_WEB_CERT_PASSWORD").ok())
        .or_else(|| file.certificate_password.clone())
        .unwrap_or_else(|| DEFAULT_CERT_PASSWORD.to_owned());

    let allow_untrusted = env_bool("UTM_WEB_ALLOW_UNTRUSTED")
        .or(file.allow_untrusted)
        .unwrap_or(false);
    let max_sessions = env::var("UTM_WEB_MAX_SESSIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(file.max_sessions)
        .unwrap_or(32);
    let session_idle_timeout_secs = env::var("UTM_WEB_SESSION_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .or(file.session_idle_timeout_secs)
        .unwrap_or(3600);
    let public_url = env::var("UTM_WEB_PUBLIC_URL")
        .ok()
        .or_else(|| file.public_url.clone());

    let profiles = ProfileStore::new(config_path.clone(), file);
    let config = AppConfig {
        listen,
        config_path,
        cert_path,
        cert_password,
        allow_untrusted,
        max_sessions: max_sessions.max(1),
        session_idle_timeout: Duration::from_secs(session_idle_timeout_secs.max(30)),
        public_url,
    };
    Ok((config, profiles))
}

pub fn ensure_client_certificate(config: &AppConfig) -> Result<crate::cert::ClientIdentity> {
    if config.cert_path.exists() {
        crate::cert::load_identity(&config.cert_path, &config.cert_password)
    } else {
        tracing::info!("generating client certificate at {:?}", config.cert_path);
        generate_identity(&config.cert_path)
    }
}

const HELP: &str = r#"Lightweight self-hosted web client for Apple UTM Remote.

USAGE:
    utm-web [OPTIONS]

OPTIONS:
    --listen <ADDR>          Listen address (default 127.0.0.1:8080)
    --config <FILE>          Configuration file (default ~/.config/utm-web/config.json)
    --cert <FILE>            Client certificate PEM (default <config dir>/client.pem)
    --cert-password <PASS>   Password for an encrypted PKCS#8 client key
    --generate-cert          Generate the client certificate and exit
    -h, --help               Show this help
    -V, --version            Show version

ENVIRONMENT:
    UTM_WEB_LISTEN, UTM_WEB_CONFIG, UTM_WEB_CERT, UTM_WEB_CERT_PASSWORD,
    UTM_WEB_ALLOW_UNTRUSTED, UTM_WEB_MAX_SESSIONS, UTM_WEB_SESSION_TIMEOUT,
    UTM_WEB_PUBLIC_URL, RUST_LOG
"#;

pub fn print_help() {
    println!("utm-web {}\n{HELP}", env!("CARGO_PKG_VERSION"));
}

pub fn config_file_exists(path: &Path) -> bool {
    path.exists()
}
