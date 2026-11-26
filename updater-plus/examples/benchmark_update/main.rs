use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use aws_lc_rs::digest;
use clap::Parser;
use semver::Version;
use serde::Serialize;
use updater_plus::{
    ArtifactVerifier, BlockMap, ChunkerConfig, DummyListener, PackageCache, UpdaterBuilder,
};

mod server;

use server::{BenchServer, PublishedHop};

const HASH_READ_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Parser)]
#[command(about = "Run a real local HTTP blockmap differential update benchmark")]
struct Args {
    /// Two or more sequential artifact paths ordered oldest to newest (rev1 rev2 [rev3 ...]).
    #[arg(required = true, num_args = 2..)]
    artifacts: Vec<PathBuf>,

    /// Optional display version labels matching each artifact path.
    #[arg(long = "version")]
    versions: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HopReport {
    from_version: String,
    to_version: String,
    mode: String,
    total_bytes: u64,
    reused_bytes: u64,
    downloaded_payload_bytes: u64,
    http_wire_bytes: u64,
    blockmap_bytes: u64,
    block_count: usize,
    http_requests: usize,
    saved_percent: f64,
    wire_saved_percent: f64,
    blockmap_gen_ms: u128,
    update_wall_ms: u128,
    expected_sha256: String,
    actual_sha256: String,
    sha256_verified: bool,
    expected_sha512: String,
    actual_sha512: String,
    sha512_verified: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChainSummary {
    hop_count: usize,
    differential_hops: usize,
    full_hops: usize,
    total_artifact_bytes: u64,
    total_reused_bytes: u64,
    total_downloaded_payload_bytes: u64,
    total_http_wire_bytes: u64,
    total_blockmap_bytes: u64,
    total_http_requests: usize,
    overall_saved_percent: f64,
    overall_wire_saved_percent: f64,
    all_hashes_verified: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    versions: Vec<String>,
    seed_old_ms: u128,
    hops: Vec<HopReport>,
    summary: ChainSummary,
}

struct HopContext {
    index: usize,
    from_label: String,
    to_label: String,
    target_path: PathBuf,
}

struct BenchHarness {
    server: BenchServer,
    cache: PackageCache,
    extract_path: PathBuf,
}

impl BenchHarness {
    async fn start(
        cache: PackageCache,
        extract_path: PathBuf,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let server = BenchServer::start().await?;
        Ok(Self {
            server,
            cache,
            extract_path,
        })
    }

    async fn run_hop(&self, hop: &HopContext) -> Result<HopReport, Box<dyn std::error::Error>> {
        let gen_start = Instant::now();
        let new_artifact =
            BlockMap::generate_from_path(&hop.target_path, ChunkerConfig::default())?;
        let new_blockmap_gz = new_artifact.blockmap.to_gzip_bytes()?;
        let gen_ms = gen_start.elapsed().as_millis();
        let expected_sha256 = sha256_file_hex(&hop.target_path)?;

        let current_ver = Version::new(0, hop.index as u64, 0);
        let next_ver = Version::new(0, (hop.index + 1) as u64, 0);
        let pkg_name = format!("artifact-{next_ver}.pkg");
        let blockmap_name = format!("{pkg_name}.blockmap");

        let manifest_json = serde_json::json!({
            "version": next_ver.to_string(),
            "notes": format!("End-to-end benchmark {} -> {}", hop.from_label, hop.to_label),
            "pub_date": "2026-11-26T00:00:00Z",
            "platforms": {
                "linux-x86_64": {
                    "url": self.server.base_url().join(&pkg_name)?,
                    "sha512": new_artifact.sha512,
                    "size": new_artifact.size,
                    "blockmap_size": new_blockmap_gz.len()
                }
            }
        });

        self.server.publish_hop(PublishedHop {
            pkg_name,
            pkg_path: hop.target_path.clone(),
            blockmap_name,
            blockmap_bytes: new_blockmap_gz.clone(),
            manifest_bytes: serde_json::to_vec(&manifest_json)?,
        });

        let updater = UpdaterBuilder::new(
            "benchmark".into(),
            current_ver,
            self.extract_path.clone(),
            self.cache.clone(),
        )
        .target("linux-x86_64")
        .endpoints(vec![self.server.base_url().join("latest.json")?])
        .build()?;

        let update_start = Instant::now();
        let update = updater
            .check()
            .await?
            .ok_or_else(|| format!("expected update {next_ver} to be available"))?;
        let downloaded = update.download(&DummyListener).await?;
        let update_ms = update_start.elapsed().as_millis();

        let actual_sha512 = ArtifactVerifier::sha512_file(&downloaded.path)?;
        let actual_sha256 = sha256_file_hex(&downloaded.path)?;
        let actual_len = std::fs::metadata(&downloaded.path)?.len();
        if actual_len != new_artifact.size {
            return Err(format!(
                "size mismatch on {} -> {}: expected {}, got {}",
                hop.from_label, hop.to_label, new_artifact.size, actual_len
            )
            .into());
        }
        let sha512_verified = actual_sha512 == new_artifact.sha512;
        let sha256_verified = actual_sha256 == expected_sha256;
        if !sha512_verified || !sha256_verified {
            return Err(format!(
                "hash verification failed on {} -> {} (sha512_ok={sha512_verified}, sha256_ok={sha256_verified})",
                hop.from_label, hop.to_label
            )
            .into());
        }

        let http_wire_bytes = self.server.wire_bytes();
        let http_requests = self.server.artifact_requests();
        let blockmap_bytes = new_blockmap_gz.len() as u64;
        let total_bytes = downloaded.summary.total_bytes;
        let wire_saved_percent = saved_percent(total_bytes, http_wire_bytes + blockmap_bytes);

        Ok(HopReport {
            from_version: hop.from_label.clone(),
            to_version: hop.to_label.clone(),
            mode: serde_json::to_value(downloaded.summary.mode)?
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            total_bytes,
            reused_bytes: downloaded.summary.reused_bytes,
            downloaded_payload_bytes: downloaded.summary.downloaded_bytes,
            http_wire_bytes,
            blockmap_bytes,
            block_count: new_artifact.blockmap.block_count(),
            http_requests,
            saved_percent: round2(downloaded.summary.saved_percent),
            wire_saved_percent: round2(wire_saved_percent),
            blockmap_gen_ms: gen_ms,
            update_wall_ms: update_ms,
            expected_sha256,
            actual_sha256,
            sha256_verified,
            expected_sha512: new_artifact.sha512,
            actual_sha512,
            sha512_verified,
        })
    }
}

fn sha256_file_hex(path: &Path) -> std::io::Result<String> {
    let mut reader = BufReader::with_capacity(HASH_READ_BUFFER_BYTES, File::open(path)?);
    let mut ctx = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; HASH_READ_BUFFER_BYTES];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let hash = ctx.finish();
    let mut hex = String::with_capacity(hash.as_ref().len() * 2);
    for byte in hash.as_ref() {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

fn saved_percent(total_bytes: u64, transferred_bytes: u64) -> f64 {
    if total_bytes == 0 {
        return 0.0;
    }
    let ratio = 1.0 - (transferred_bytes as f64 / total_bytes as f64);
    ratio * 100.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn resolve_labels(args: &Args) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if args.versions.is_empty() {
        return Ok((1..=args.artifacts.len())
            .map(|idx| format!("rev{idx}"))
            .collect());
    }
    if args.versions.len() != args.artifacts.len() {
        return Err(format!(
            "--version count ({}) must match artifact count ({})",
            args.versions.len(),
            args.artifacts.len()
        )
        .into());
    }
    Ok(args.versions.clone())
}

fn build_summary(hops: &[HopReport]) -> ChainSummary {
    let differential_hops = hops.iter().filter(|h| h.mode == "differential").count();
    let full_hops = hops.len().saturating_sub(differential_hops);
    let total_artifact_bytes: u64 = hops.iter().map(|h| h.total_bytes).sum();
    let total_reused_bytes: u64 = hops.iter().map(|h| h.reused_bytes).sum();
    let total_downloaded_payload_bytes: u64 = hops.iter().map(|h| h.downloaded_payload_bytes).sum();
    let total_http_wire_bytes: u64 = hops.iter().map(|h| h.http_wire_bytes).sum();
    let total_blockmap_bytes: u64 = hops.iter().map(|h| h.blockmap_bytes).sum();
    let total_http_requests: usize = hops.iter().map(|h| h.http_requests).sum();
    let overall_saved_percent = round2(saved_percent(
        total_artifact_bytes,
        total_downloaded_payload_bytes,
    ));
    let overall_wire_saved_percent = round2(saved_percent(
        total_artifact_bytes,
        total_http_wire_bytes + total_blockmap_bytes,
    ));
    let all_hashes_verified = hops.iter().all(|h| h.sha256_verified && h.sha512_verified);

    ChainSummary {
        hop_count: hops.len(),
        differential_hops,
        full_hops,
        total_artifact_bytes,
        total_reused_bytes,
        total_downloaded_payload_bytes,
        total_http_wire_bytes,
        total_blockmap_bytes,
        total_http_requests,
        overall_saved_percent,
        overall_wire_saved_percent,
        all_hashes_verified,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let labels = resolve_labels(&args)?;

    let temp_dir = tempfile::tempdir()?;
    let cache = PackageCache::new(temp_dir.path().join("cache"));
    let seed_start = Instant::now();
    cache.seed_from_file(&args.artifacts[0], "0.1.0")?;
    let seed_old_ms = seed_start.elapsed().as_millis();

    let harness = BenchHarness::start(cache, temp_dir.path().join("App.pkg")).await?;
    let mut hops = Vec::with_capacity(args.artifacts.len() - 1);
    for idx in 1..args.artifacts.len() {
        let hop = HopContext {
            index: idx,
            from_label: labels[idx - 1].clone(),
            to_label: labels[idx].clone(),
            target_path: args.artifacts[idx].clone(),
        };
        hops.push(harness.run_hop(&hop).await?);
    }

    let summary = build_summary(&hops);
    let report = BenchmarkReport {
        versions: labels,
        seed_old_ms,
        hops,
        summary,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
