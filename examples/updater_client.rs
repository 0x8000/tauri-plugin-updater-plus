use std::path::PathBuf;
use std::sync::Mutex;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::{Parser, Subcommand};
use semver::Version;
use serde::{Deserialize, Serialize};
use tauri_plugin_updater_plus::{
    Config, DownloadEvent, DownloadListener, DownloadSummary, Error, PackageCache, ReleaseManifest,
    UpdaterBuilder,
};
use url::Url;

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunSpec {
    action: String,
    app_name: String,
    current_version: Version,
    extract_path: PathBuf,
    cache_dir: PathBuf,
    #[serde(default)]
    appimage_fallback: Option<PathBuf>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    config: Config,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunReport {
    status: &'static str,
    update_available: bool,
    version: Option<String>,
    notes: Option<String>,
    summary: Option<DownloadSummary>,
    events_count: usize,
    started_differential: Option<bool>,
    progress_bytes_total: usize,
    has_finished_event: bool,
    cached_version: Option<String>,
    cached_size: Option<u64>,
}

#[derive(Default)]
struct RecordingListener {
    events: Mutex<Vec<DownloadEvent>>,
}

impl DownloadListener for RecordingListener {
    fn on_event(&self, event: DownloadEvent) {
        self.events.lock().expect("Mutex poisoned").push(event);
    }
}

fn decode_hex(hex: &str, expected_len: usize) -> std::result::Result<Vec<u8>, String> {
    let trimmed = hex.trim();
    if trimmed.len() != expected_len * 2 {
        return Err(format!(
            "expected {} hex chars, got {}",
            expected_len * 2,
            trimmed.len()
        ));
    }
    (0..expected_len)
        .map(|idx| {
            u8::from_str_radix(&trimmed[idx * 2..idx * 2 + 2], 16)
                .map_err(|err| format!("invalid hex byte at {idx}: {err}"))
        })
        .collect()
}

fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::InvalidConfig(_) => "InvalidConfig",
        Error::InvalidUrl(_) => "InvalidUrl",
        Error::InvalidHeader(_) => "InvalidHeader",
        Error::Http(_) => "Http",
        Error::HttpStatus { .. } => "HttpStatus",
        Error::InvalidManifest(_) => "InvalidManifest",
        Error::TargetNotFound(_) => "TargetNotFound",
        Error::InvalidBlockMap(_) => "InvalidBlockMap",
        Error::DifferentialTransfer(_) => "DifferentialTransfer",
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
        Error::Tauri(_) => "Tauri",
    }
}

async fn execute_run(spec_path: PathBuf) -> tauri_plugin_updater_plus::Result<RunReport> {
    let raw = std::fs::read_to_string(&spec_path)?;
    let spec: RunSpec = serde_json::from_str(&raw)?;
    let cache = PackageCache::with_appimage_fallback(spec.cache_dir, spec.appimage_fallback);

    let mut builder = UpdaterBuilder::new(
        spec.app_name,
        spec.current_version,
        spec.extract_path,
        cache.clone(),
    )
    .config(spec.config);

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
        Commands::Keygen { seed_hex } => {
            let seed = decode_hex(&seed_hex, 32).expect("valid 32-byte seed hex");
            let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("ed25519 keypair");
            let key_id = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
            let mut pub_bin = Vec::with_capacity(42);
            pub_bin.extend_from_slice(b"Ed");
            pub_bin.extend_from_slice(&key_id);
            pub_bin.extend_from_slice(key_pair.public_key().as_ref());
            let pubkey_text = format!(
                "untrusted comment: minisign public key\n{}",
                STANDARD.encode(&pub_bin)
            );
            println!(
                "{}",
                serde_json::json!({ "pubkey": STANDARD.encode(pubkey_text) })
            );
        }
        Commands::Sign {
            seed_hex,
            blake2b_hex,
            trusted_comment,
        } => {
            let seed = decode_hex(&seed_hex, 32).expect("valid 32-byte seed hex");
            let digest = decode_hex(&blake2b_hex, 64).expect("valid 64-byte blake2b hex");
            let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("ed25519 keypair");
            let key_id = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

            let sig_bytes = key_pair.sign(&digest);
            let mut sig_bin = Vec::with_capacity(74);
            sig_bin.extend_from_slice(b"ED");
            sig_bin.extend_from_slice(&key_id);
            sig_bin.extend_from_slice(sig_bytes.as_ref());

            let mut global_msg = Vec::with_capacity(64 + trusted_comment.len());
            global_msg.extend_from_slice(sig_bytes.as_ref());
            global_msg.extend_from_slice(trusted_comment.as_bytes());
            let global_sig = key_pair.sign(&global_msg);

            let sig_text = format!(
                "untrusted comment: minisign signature\n{}\ntrusted comment: {trusted_comment}\n{}",
                STANDARD.encode(&sig_bin),
                STANDARD.encode(global_sig.as_ref()),
            );
            println!(
                "{}",
                serde_json::json!({ "signature": STANDARD.encode(sig_text) })
            );
        }
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
