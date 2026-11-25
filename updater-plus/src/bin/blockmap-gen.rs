use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use semver::Version;
use time::OffsetDateTime;
use updater_plus::{
    BlockMap, ChunkerConfig, DownloadPlan, PlanOptions, ReleaseManifest, ReleasePlatform,
    ReleasePlatforms,
};
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
    fn run(self) -> updater_plus::Result<()> {
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

    fn pack_directory_zip(root: &Path, output: &Path) -> updater_plus::Result<()> {
        let file = File::create(output)?;
        let mut writer = ZipWriter::new(file);
        let prefix_base = if root
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            root.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
        } else {
            root
        };
        let mut paths = Vec::new();
        Self::collect_sorted_entries(prefix_base, root, &mut paths)?;
        paths.sort();

        let base_options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        let mut scratch = vec![0u8; 256 * 1024];

        for (rel, full) in paths {
            let name = rel.to_string_lossy().replace('\\', "/");
            let meta = std::fs::symlink_metadata(&full)?;
            #[cfg(unix)]
            let options = {
                use std::os::unix::fs::PermissionsExt;
                base_options.unix_permissions(meta.permissions().mode())
            };
            #[cfg(not(unix))]
            let options = base_options;

            if meta.file_type().is_symlink() {
                let link_target = std::fs::read_link(&full)?;
                let target_str = link_target.to_string_lossy().replace('\\', "/");
                writer.add_symlink(name, target_str, options)?;
                continue;
            }
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

    fn collect_sorted_entries(
        prefix_base: &Path,
        current: &Path,
        out: &mut Vec<(PathBuf, PathBuf)>,
    ) -> std::io::Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(current)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            let file_type = std::fs::symlink_metadata(&path)?.file_type();
            if file_type.is_dir() {
                Self::collect_sorted_entries(prefix_base, &path, out)?;
            } else if file_type.is_file() || file_type.is_symlink() {
                let rel = path
                    .strip_prefix(prefix_base)
                    .unwrap_or(&path)
                    .to_path_buf();
                out.push((rel, path));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_zip_preserves_app_bundle_root_permissions_and_symlinks() {
        let temp = tempfile::tempdir().expect("temp dir");
        let app_dir = temp.path().join("MyApp.app");
        let macos_dir = app_dir.join("Contents/MacOS");
        let fw_dir = app_dir.join("Contents/Frameworks/Core.framework/Versions/A");
        std::fs::create_dir_all(&macos_dir).expect("mkdir MacOS");
        std::fs::create_dir_all(&fw_dir).expect("mkdir Frameworks");

        let bin_path = macos_dir.join("MyApp");
        std::fs::write(&bin_path, b"#!/bin/sh\necho hi\n").expect("write bin");
        std::fs::write(fw_dir.join("Core"), b"dylib-bytes").expect("write fw");

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            symlink(
                "Versions/A/Core",
                app_dir.join("Contents/Frameworks/Core.framework/Core"),
            )
            .expect("symlink");
        }

        let out_zip = temp.path().join("MyApp.app.zip");
        Cli::pack_directory_zip(&app_dir, &out_zip).expect("pack zip");

        let mut archive =
            zip::ZipArchive::new(File::open(&out_zip).expect("open zip")).expect("zip archive");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_string())
            .collect();
        assert!(
            names.iter().all(|n| n.starts_with("MyApp.app/")),
            "every entry in packed .app zip must keep MyApp.app/ prefix: {names:?}"
        );
        assert!(names.contains(&"MyApp.app/Contents/MacOS/MyApp".to_string()));

        #[cfg(unix)]
        {
            let bin_entry = archive
                .by_name("MyApp.app/Contents/MacOS/MyApp")
                .expect("bin entry");
            assert_eq!(bin_entry.unix_mode().expect("mode") & 0o777, 0o755);
            drop(bin_entry);

            let mut link_entry = archive
                .by_name("MyApp.app/Contents/Frameworks/Core.framework/Core")
                .expect("symlink entry");
            assert!(link_entry.is_symlink(), "framework link must stay symlink");
            let mut link_target = String::new();
            link_entry.read_to_string(&mut link_target).expect("read");
            assert_eq!(link_target, "Versions/A/Core");
        }
    }

    #[test]
    fn pack_zip_strips_plain_dir_prefix_is_deterministic_and_errors_on_missing_dir() {
        let temp = tempfile::tempdir().expect("temp dir");
        let plain_dir = temp.path().join("payload");
        std::fs::create_dir_all(plain_dir.join("sub")).expect("mkdir");
        std::fs::write(plain_dir.join("b.txt"), b"beta").expect("write b");
        std::fs::write(plain_dir.join("sub/a.txt"), b"alpha").expect("write a");

        let zip1 = temp.path().join("one.zip");
        let zip2 = temp.path().join("two.zip");
        Cli::pack_directory_zip(&plain_dir, &zip1).expect("pack 1");
        Cli::pack_directory_zip(&plain_dir, &zip2).expect("pack 2");
        assert_eq!(
            std::fs::read(&zip1).expect("read zip 1"),
            std::fs::read(&zip2).expect("read zip 2")
        );

        let mut archive =
            zip::ZipArchive::new(File::open(&zip1).expect("open zip")).expect("zip archive");
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).expect("entry").name().to_string())
            .collect();
        assert_eq!(names, vec!["b.txt", "sub/a.txt"]);

        let missing = temp.path().join("does-not-exist");
        let err_zip = temp.path().join("err.zip");
        assert!(Cli::pack_directory_zip(&missing, &err_zip).is_err());
    }
}
