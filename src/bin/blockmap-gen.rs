use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use semver::Version;
use tauri_plugin_updater_plus::{
    BlockMap, ChunkerConfig, DownloadPlan, PlanOptions, ReleaseManifest, ReleasePlatform,
    ReleasePlatforms,
};
use time::OffsetDateTime;
use url::Url;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Parser)]
#[command(
    name = "blockmap-gen",
    version,
    about = "Generate Electron-compatible v2 blockmaps, manifests, and differential reports"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum ManifestFormat {
    Tauri,
    Electron,
}

#[derive(clap::Args)]
struct ManifestArgs {
    #[arg(short, long)]
    input: PathBuf,
    #[arg(long)]
    version: Version,
    #[arg(long)]
    url: Url,
    #[arg(long, value_enum, default_value_t = ManifestFormat::Tauri)]
    format: ManifestFormat,
    #[arg(long, default_value = "linux-x86_64")]
    target: String,
    #[arg(long)]
    signature: Option<String>,
    #[arg(long)]
    notes: Option<String>,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long)]
    blockmap_output: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a gzipped Electron v2 `.blockmap` for an update artifact.
    Generate {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 16 * 1024)]
        min_size: usize,
        #[arg(long, default_value_t = 64 * 1024)]
        avg_size: usize,
        #[arg(long, default_value_t = 256 * 1024)]
        max_size: usize,
    },
    /// Generate a `.blockmap` and a Tauri `latest.json` or Electron `latest*.yml` manifest.
    Manifest(Box<ManifestArgs>),
    /// Compare two `.blockmap` files and print differential download efficiency metrics.
    Diff {
        #[arg(long)]
        old_blockmap: PathBuf,
        #[arg(long)]
        new_blockmap: PathBuf,
        #[arg(long, default_value_t = 0)]
        max_range_gap: u64,
    },
    /// Pack a directory into a per-entry deflated ZIP friendly to blockmap differential updates.
    PackZip {
        #[arg(short, long)]
        dir: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = cli.run() {
        eprintln!("blockmap-gen failed: {error}");
        std::process::exit(1);
    }
}

impl Cli {
    fn run(self) -> tauri_plugin_updater_plus::Result<()> {
        match self.command {
            Commands::Generate {
                input,
                output,
                min_size,
                avg_size,
                max_size,
            } => {
                let config = ChunkerConfig {
                    min_size,
                    avg_size,
                    max_size,
                };
                let artifact = BlockMap::generate_from_path(&input, config)?;
                let encoded = artifact.blockmap.to_gzip_bytes()?;
                let out_path = output.unwrap_or_else(|| Self::default_blockmap_path(&input));
                std::fs::write(&out_path, &encoded)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "input": input,
                        "blockmap": out_path,
                        "size": artifact.size,
                        "sha512": artifact.sha512,
                        "blockmapSize": encoded.len(),
                        "blockCount": artifact.blockmap.block_count(),
                    }))?
                );
                Ok(())
            }
            Commands::Manifest(args) => {
                let ManifestArgs {
                    input,
                    version,
                    url,
                    format,
                    target,
                    signature,
                    notes,
                    output,
                    blockmap_output,
                } = *args;
                let artifact = BlockMap::generate_from_path(&input, ChunkerConfig::default())?;
                let encoded = artifact.blockmap.to_gzip_bytes()?;
                let map_path =
                    blockmap_output.unwrap_or_else(|| Self::default_blockmap_path(&input));
                std::fs::write(&map_path, &encoded)?;

                let platform = ReleasePlatform {
                    url,
                    signature,
                    sha512: Some(artifact.sha512),
                    size: Some(artifact.size),
                    blockmap_url: None,
                    blockmap_size: Some(encoded.len() as u64),
                };
                let mut map = HashMap::new();
                map.insert(target.clone(), platform);
                let manifest = ReleaseManifest {
                    version,
                    notes,
                    pub_date: Some(OffsetDateTime::now_utc()),
                    platforms: ReleasePlatforms::Map(map),
                    raw_json: serde_json::Value::Null,
                };
                let content = match format {
                    ManifestFormat::Tauri => manifest.to_tauri_json()?,
                    ManifestFormat::Electron => manifest.to_electron_yaml(&[target])?,
                };
                std::fs::write(&output, content)?;
                println!("wrote {} and {}", output.display(), map_path.display());
                Ok(())
            }
            Commands::Diff {
                old_blockmap,
                new_blockmap,
                max_range_gap,
            } => {
                let old_map = BlockMap::from_bytes(&std::fs::read(&old_blockmap)?)?;
                let new_map = BlockMap::from_bytes(&std::fs::read(&new_blockmap)?)?;
                let plan = DownloadPlan::build(&old_map, &new_map, PlanOptions { max_range_gap })?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "totalBytes": plan.total_size,
                        "reusedBytes": plan.reused_bytes,
                        "downloadBytes": plan.download_bytes,
                        "rangeCount": plan.range_count(),
                        "savedPercent": (plan.saved_percent() * 100.0).round() / 100.0,
                    }))?
                );
                Ok(())
            }
            Commands::PackZip { dir, output } => {
                Self::pack_directory_zip(&dir, &output)?;
                println!("packed {}", output.display());
                Ok(())
            }
        }
    }

    fn default_blockmap_path(input: &Path) -> PathBuf {
        let mut name = input.as_os_str().to_os_string();
        name.push(".blockmap");
        PathBuf::from(name)
    }

    fn pack_directory_zip(root: &Path, output: &Path) -> tauri_plugin_updater_plus::Result<()> {
        let file = File::create(output)?;
        let mut writer = ZipWriter::new(file);
        let mut paths = Vec::new();
        Self::collect_sorted_files(root, root, &mut paths)?;
        paths.sort();

        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        let mut scratch = vec![0u8; 256 * 1024];

        for (rel, full) in paths {
            let name = rel.to_string_lossy().replace('\\', "/");
            writer.start_file(name, options)?;
            let mut src = File::open(full)?;
            loop {
                let n = src.read(&mut scratch)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&scratch[..n])?;
            }
        }
        writer.finish()?;
        Ok(())
    }

    fn collect_sorted_files(
        root: &Path,
        current: &Path,
        out: &mut Vec<(PathBuf, PathBuf)>,
    ) -> std::io::Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(current)?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                Self::collect_sorted_files(root, &path, out)?;
            } else if path.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.push((rel, path));
            }
        }
        Ok(())
    }
}
