# updater-plus

[![updater-plus crates.io](https://img.shields.io/crates/v/updater-plus.svg?label=updater-plus)](https://crates.io/crates/updater-plus)
[![tauri-plugin-updater-plus crates.io](https://img.shields.io/crates/v/tauri-plugin-updater-plus.svg?label=tauri-plugin-updater-plus)](https://crates.io/crates/tauri-plugin-updater-plus)
[![docs.rs](https://docs.rs/updater-plus/badge.svg)](https://docs.rs/updater-plus)

Differential updates powered by an Electron-style blockmap engine, split into two workspace crates:

- [`updater-plus`](updater-plus): framework-agnostic differential download & verification engine, pluggable `UpdateProvider` / `UpdateInstaller` traits (with optional `native-installer` facade: `DesktopInstaller`, `AndroidInstaller`, `PlatformInstaller`), and the `blockmap-gen` CLI.
- [`tauri-plugin-updater-plus`](tauri-plugin-updater-plus): Tauri v3 plugin, IPC commands, Android native bridge, and TypeScript guest bindings.

| Platform | Supported |
| -------- | --------- |
| Linux    | ✓         |
| Windows  | ✓         |
| macOS    | ✓         |
| Android  | ✓         |
| iOS      | x         |

## Install

_This workspace requires a Rust version of at least **1.95**._

For Tauri v3 applications, add `tauri-plugin-updater-plus` to `src-tauri/Cargo.toml`:

```toml
# add to `[dependencies]` to target Linux, Windows, macOS, and Android
[dependencies]
tauri-plugin-updater-plus = "0.4"
# alternatively with Git:
tauri-plugin-updater-plus = { git = "https://github.com/0x8000/updater-plus", branch = "master" }
```

For non-Tauri applications (or custom hot-update flows such as web bundles or Lua scripts), depend directly on `updater-plus` without Tauri:

```toml
[dependencies]
# Differential HTTP engine without OS installer dependencies:
updater-plus = { version = "0.4", default-features = false, features = ["http"] }
```

The TypeScript guest bindings (built on `@tauri-apps/api/core`) are in [`tauri-plugin-updater-plus/guest-js/index.ts`](tauri-plugin-updater-plus/guest-js/index.ts) and can be imported directly into your frontend project.

## Usage

First, register the core plugin with Tauri:

`src-tauri/src/lib.rs`

```rust
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(not(target_os = "ios"))]
            app.handle()
                .plugin(tauri_plugin_updater_plus::Builder::new().build())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Configure update endpoints in `src-tauri/tauri.conf.json`. Use `plugins.updater` when `bundle.createUpdaterArtifacts` is enabled so Tauri CLI's updater bundling and `tauri-plugin-updater-plus` share a single configuration (`plugins.updater-plus` is also supported standalone or as an overlay on top of `plugins.updater`):

```json
{
  "bundle": {
    "createUpdaterArtifacts": true
  },
  "plugins": {
    "updater": {
      "pubkey": "<base64-minisign-public-key>",
      "differential": true,
      "endpoints": [
        "https://github.com/owner/repo/releases/latest/download/latest.json"
      ]
    }
  }
}
```

Enable the default permissions in `src-tauri/capabilities/default.json`:

```json
{
  "permissions": [
    "updater-plus:default"
  ]
}
```

Afterwards, all plugin APIs are available through the JavaScript/TypeScript guest bindings:

```ts
import { check } from './guest-js'
import { relaunch } from '@tauri-apps/plugin-process'

const update = await check()
if (update) {
  await update.downloadAndInstall()
  // Relaunch the app on macOS and Linux to run the newly installed version
  // (Android hands off to the system package installer instead of relaunching).
  await relaunch()
}
```

On Android, the plugin resolves `android-{arch}-apk`, `android-{arch}`, `android-universal`, and `android` manifest targets, uses the installed base APK as a differential blockmap cache fallback, and hands verified `.apk` packages to the system package installer via a dedicated `FileProvider` (requesting `REQUEST_INSTALL_PACKAGES` on Android 8+ when needed).

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

# Pack a macOS .app bundle into a differential-friendly ZIP
blockmap-gen pack-zip --dir MyApp.app --output MyApp.app.zip
```

Upload the release artifact, `<artifact>.blockmap`, and manifest (`latest.json` or `latest*.yml`) together to your static server, GitHub Releases, or object store (S3 / R2 / OSS / GCS).

## Contributing

PRs accepted.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
