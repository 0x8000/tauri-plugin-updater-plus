# updater-plus

[![updater-plus crates.io](https://img.shields.io/crates/v/updater-plus.svg?label=updater-plus)](https://crates.io/crates/updater-plus)
[![tauri-plugin-updater-plus crates.io](https://img.shields.io/crates/v/tauri-plugin-updater-plus.svg?label=tauri-plugin-updater-plus)](https://crates.io/crates/tauri-plugin-updater-plus)
[![docs.rs](https://docs.rs/updater-plus/badge.svg)](https://docs.rs/updater-plus)

Framework-agnostic Electron-style blockmap differential update engine, pluggable installer framework, `blockmap-gen` CLI, and Tauri v3 plugin.

## Workspace Crates

| Crate | Purpose | Docs |
| ----- | ------- | ---- |
| [`updater-plus`](updater-plus) | Core FastCDC blockmap differential download, verification, caching, pluggable `UpdateProvider` & `UpdateInstaller` traits, optional OS-native installers (`DesktopInstaller`, `AndroidInstaller`, `PlatformInstaller`), and `blockmap-gen` CLI | [`updater-plus/README.md`](updater-plus/README.md) |
| [`tauri-plugin-updater-plus`](tauri-plugin-updater-plus) | Tauri v3 plugin wrapper with IPC commands, permissions, Android native bridge, and TypeScript guest bindings | [`tauri-plugin-updater-plus/README.md`](tauri-plugin-updater-plus/README.md) |

## Features (`updater-plus`)

_This workspace requires a Rust version of at least **1.95**._

- `default = ["http", "native-installer"]`: HTTP/GitHub/S3 providers plus OS-native installers (`DesktopInstaller`, `AndroidInstaller`, `PlatformInstaller`).
- `engine`: transport-agnostic differential core (`FastCdc`, `BlockMap`, `DownloadPlan`, `DifferentialDownloader`, `PackageCache`, `ArtifactVerifier`, `UpdateProvider`, `UpdateInstaller`, `Updater`) with zero Tauri or OS installer dependencies.
- `http`: built-in HTTP, GitHub Releases, and S3 providers on top of `engine`.
- `native-installer`: OS-native `DesktopInstaller` (Linux AppImage/deb/rpm, macOS `.app`, Windows NSIS/MSI), `AndroidInstaller`, and `PlatformInstaller` facade.
- `cli`: builds the `blockmap-gen` CLI binary.

## Quick Start

### Differential Engine Only (Web Bundles, Lua Scripts, Custom Apps)

Depend on `updater-plus` without native OS installers or Tauri:

```toml
[dependencies]
updater-plus = { version = "0.5", default-features = false, features = ["http"] }
```

Implement `UpdateInstaller` to apply downloaded and verified packages however your host runtime requires (e.g., versioned directory extraction, web asset reload, or Lua script hot-swap):

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use semver::Version;
use updater_plus::{
    DummyListener, InstallContext, PackageCache, Result, UpdateInstaller, UpdaterBuilder,
};
use url::Url;

#[derive(Debug)]
struct CustomBundleInstaller;

impl UpdateInstaller for CustomBundleInstaller {
    fn install(&self, package_path: &Path, ctx: &InstallContext) -> Result<()> {
        // Deploy verified archive at `package_path` for `ctx.version`
        let _ = (package_path, ctx);
        Ok(())
    }
}

async fn run_update(cache_dir: PathBuf, endpoint: Url) -> Result<()> {
    let updater = UpdaterBuilder::new(
        "my-bundle".into(),
        Version::new(0, 1, 0),
        cache_dir.join("active"),
        PackageCache::new(cache_dir),
    )
    .target("web-bundle")
    .endpoints(vec![endpoint])
    .installer(Arc::new(CustomBundleInstaller))
    .build()?;

    if let Some(update) = updater.check().await? {
        update.download_and_install(&DummyListener).await?;
    }
    Ok(())
}
```

### OS-Native App Updates

With default features (`http` + `native-installer`), `UpdaterBuilder` uses `PlatformInstaller::current()` automatically (delegating to `DesktopInstaller` on Linux/macOS/Windows, `AndroidInstaller` when configured with an `AndroidApkLauncher` on Android, and `UnsupportedInstaller` on unsupported targets).

For **Tauri v3** applications, see [`tauri-plugin-updater-plus/README.md`](tauri-plugin-updater-plus/README.md).

## Generating Release Artifacts (`blockmap-gen`)

To publish differential updates, generate a `.blockmap` and manifest alongside your release files using `blockmap-gen`.

Download prebuilt binaries from [GitHub Releases](https://github.com/0x8000/updater-plus/releases), or install via Cargo:

```sh
cargo install updater-plus --no-default-features --features cli
```

```sh
# Generate a .blockmap and Tauri/Electron release manifest
blockmap-gen manifest \
  --input MyApp-0.2.0.AppImage \
  --version 0.2.0 \
  --url https://example.com/MyApp-0.2.0.AppImage \
  --format tauri \
  --target linux-x86_64 \
  --output latest.json

# Generate only a .blockmap file
blockmap-gen generate --input MyApp.AppImage

# Pack a directory (macOS .app bundle, web dist/, or Lua scripts) into a differential-friendly ZIP
blockmap-gen pack-zip --dir MyApp.app --output MyApp.app.zip
```

Upload the release artifact, `<artifact>.blockmap`, and manifest (`latest.json` or `latest*.yml`) together to your static server, GitHub Releases, or object store (S3 / R2 / OSS / GCS).

## Contributing

PRs accepted.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
