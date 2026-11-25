use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Parser, Subcommand};
use semver::Version;
use serde::{Deserialize, Serialize};
use updater_plus::{
    Config, DownloadEvent, DownloadListener, DownloadSummary, Error, PackageCache, ReleaseManifest,
    Result, UpdaterBuilder,
};
use url::Url;

mod crypto;
mod mqtt;
mod mqtt_wire;
mod providers;

#[derive(Parser)]
#[command(
    name = "updater_client",
    about = "End-to-end CLI harness driving the tauri-plugin-updater-plus public API"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Derive a deterministic minisign Ed25519 public key from a 32-byte hex seed.
    Keygen {
        #[arg(long)]
        seed_hex: String,
    },
    /// Produce a prehashed minisign signature over a 64-byte BLAKE2b-512 hex digest.
    Sign {
        #[arg(long)]
        seed_hex: String,
        #[arg(long)]
        blake2b_hex: String,
        #[arg(long)]
        trusted_comment: String,
    },
    /// Seed a local PackageCache directory from an existing artifact file.
    SeedCache {
        #[arg(long)]
        cache_dir: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        version: String,
    },
    /// Execute check, download, or install against a JSON run specification.
    Run {
        #[arg(long)]
        spec: PathBuf,
    },
    /// Parse a Tauri JSON or Electron YAML manifest and resolve a target platform entry.
    ParseManifest {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        source_url: Url,
        #[arg(long, default_value = "linux-x86_64")]
        target: String,
    },
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderSpec {
    #[serde(default = "default_provider_kind")]
    pub(crate) kind: String,
    pub(crate) broker_addr: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) repo: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) api_base_url: Option<Url>,
    pub(crate) manifest_name: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) access_key_id: Option<String>,
    pub(crate) secret_access_key: Option<String>,
    pub(crate) session_token: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) max_retries: Option<usize>,
}

fn default_provider_kind() -> String {
    "static_http".into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunSpec {
    pub(crate) action: String,
    pub(crate) app_name: String,
    pub(crate) current_version: Version,
    pub(crate) extract_path: PathBuf,
    pub(crate) cache_dir: PathBuf,
    #[serde(default)]
    pub(crate) appimage_fallback: Option<PathBuf>,
    #[serde(default)]
    pub(crate) target: Option<String>,
    #[serde(default)]
    pub(crate) headers: Vec<(String, String)>,
    #[serde(default)]
    pub(crate) provider: ProviderSpec,
    pub(crate) config: Config,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunReport {
    pub(crate) status: &'static str,
    pub(crate) update_available: bool,
    pub(crate) version: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) summary: Option<DownloadSummary>,
    pub(crate) events_count: usize,
    pub(crate) started_differential: Option<bool>,
    pub(crate) progress_bytes_total: usize,
    pub(crate) has_finished_event: bool,
    pub(crate) cached_version: Option<String>,
    pub(crate) cached_size: Option<u64>,
}

#[derive(Default)]
pub(crate) struct RecordingListener {
    pub(crate) events: Mutex<Vec<DownloadEvent>>,
}

impl DownloadListener for RecordingListener {
    fn on_event(&self, event: DownloadEvent) {
        self.events.lock().expect("Mutex poisoned").push(event);
    }
}

fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::InvalidConfig(_) => "InvalidConfig",
        Error::InvalidUrl(_) => "InvalidUrl",
        Error::InvalidHeader(_) => "InvalidHeader",
        Error::Http(_) => "Http",
        Error::HttpStatus { .. } => "HttpStatus",
        Error::Transport(_) => "Transport",
        Error::InvalidManifest(_) => "InvalidManifest",
        Error::TargetNotFound(_) => "TargetNotFound",
        Error::InvalidBlockMap(_) => "InvalidBlockMap",
        Error::DifferentialTransfer(_) => "DifferentialTransfer",
        Error::IncompleteTransfer { .. } => "IncompleteTransfer",
        Error::ChecksumMismatch { .. } => "ChecksumMismatch",
        Error::Signature(_) => "Signature",
        Error::SignedVersionMismatch { .. } => "SignedVersionMismatch",
        Error::MissingSignedVersion => "MissingSignedVersion",
        Error::InvalidPackageFormat(_) => "InvalidPackageFormat",
        Error::Install(_) => "Install",
        Error::UnsupportedPlatform => "UnsupportedPlatform",
        Error::Io(_) => "Io",
        Error::Json(_) => "Json",
        Error::Zip(_) => "Zip",
        Error::Semver(_) => "Semver",
    }
}

async fn execute_run(spec_path: PathBuf) -> Result<RunReport> {
    let raw = std::fs::read_to_string(&spec_path)?;
    let spec: RunSpec = serde_json::from_str(&raw)?;
    let cache = PackageCache::with_appimage_fallback(
        spec.cache_dir.clone(),
        spec.appimage_fallback.clone(),
    );

    if spec.provider.kind == "mqtt_push" {
        return mqtt::execute_mqtt_push(spec, cache).await;
    }

    let custom_provider = providers::build_custom_provider(&spec)?;
    let mut builder = UpdaterBuilder::new(
        spec.app_name,
        spec.current_version,
        spec.extract_path,
        cache.clone(),
    )
    .config(spec.config);

    if let Some(provider) = custom_provider {
        builder = builder.provider(provider);
    }
    if let Some(target) = spec.target {
        builder = builder.target(target);
    }
    for (key, value) in &spec.headers {
        builder = builder.header(key, value)?;
    }

    let updater = builder.build()?;
    let Some(update) = updater.check().await? else {
        let state = cache.state();
        return Ok(RunReport {
            status: "ok",
            update_available: false,
            version: None,
            notes: None,
            summary: None,
            events_count: 0,
            started_differential: None,
            progress_bytes_total: 0,
            has_finished_event: false,
            cached_version: state.as_ref().map(|s| s.version.clone()),
            cached_size: state.as_ref().map(|s| s.size),
        });
    };

    let version = Some(update.version.clone());
    let notes = update.body.clone();
    let listener = RecordingListener::default();

    let summary = match spec.action.as_str() {
        "check" => None,
        "download" => Some(update.download(&listener).await?.summary),
        "download_and_install" => Some(update.download_and_install(&listener).await?),
        "download_then_install" => {
            let downloaded = update.download(&listener).await?;
            update.install_file(&downloaded.path)?;
            Some(downloaded.summary)
        }
        other => {
            return Err(Error::InvalidConfig(format!(
                "unsupported run action '{other}'"
            )));
        }
    };

    let events = listener.events.lock().expect("Mutex poisoned");
    let started_differential = match events.first() {
        Some(DownloadEvent::Started { differential, .. }) => Some(*differential),
        _ => None,
    };
    let progress_bytes_total = events
        .iter()
        .filter_map(|event| match event {
            DownloadEvent::Progress { chunk_length } => Some(*chunk_length),
            _ => None,
        })
        .fold(0usize, usize::saturating_add);
    let has_finished_event = matches!(events.last(), Some(DownloadEvent::Finished { .. }));

    let state = cache.state();
    Ok(RunReport {
        status: "ok",
        update_available: true,
        version,
        notes,
        summary,
        events_count: events.len(),
        started_differential,
        progress_bytes_total,
        has_finished_event,
        cached_version: state.as_ref().map(|s| s.version.clone()),
        cached_size: state.as_ref().map(|s| s.size),
    })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Keygen { seed_hex } => crypto::run_keygen(&seed_hex),
        Commands::Sign {
            seed_hex,
            blake2b_hex,
            trusted_comment,
        } => crypto::run_sign(&seed_hex, &blake2b_hex, &trusted_comment),
        Commands::SeedCache {
            cache_dir,
            input,
            version,
        } => {
            let cache = PackageCache::with_appimage_fallback(cache_dir, None);
            match cache.seed_from_file(&input, &version) {
                Ok(_) => {
                    let state = cache.state().expect("seeded cache state");
                    println!(
                        "{}",
                        serde_json::to_string(&state).expect("serialize state")
                    );
                }
                Err(err) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "error",
                            "errorKind": error_kind(&err),
                            "message": err.to_string(),
                        })
                    );
                    std::process::exit(1);
                }
            }
        }
        Commands::Run { spec } => match execute_run(spec).await {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string(&report).expect("serialize report")
                );
            }
            Err(err) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "errorKind": error_kind(&err),
                        "message": err.to_string(),
                    })
                );
                std::process::exit(1);
            }
        },
        Commands::ParseManifest {
            input,
            source_url,
            target,
        } => {
            let raw = std::fs::read_to_string(&input).expect("read manifest");
            let manifest = ReleaseManifest::parse(&raw, &source_url).expect("parse manifest");
            let platform = manifest
                .resolve_platform(&[target])
                .expect("resolve platform");
            println!(
                "{}",
                serde_json::json!({
                    "version": manifest.version.to_string(),
                    "notes": manifest.notes,
                    "url": platform.url.as_str(),
                    "sha512": platform.sha512,
                    "size": platform.size,
                    "blockmapSize": platform.blockmap_size,
                })
            );
        }
    }
}
