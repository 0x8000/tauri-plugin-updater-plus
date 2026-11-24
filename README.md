# tauri-plugin-updater-plus

[![crates.io](https://img.shields.io/crates/v/tauri-plugin-updater-plus.svg)](https://crates.io/crates/tauri-plugin-updater-plus)
[![docs.rs](https://docs.rs/tauri-plugin-updater-plus/badge.svg)](https://docs.rs/tauri-plugin-updater-plus)

Differential in-app updates for Tauri v3 applications.

| Platform | Supported |
| -------- | --------- |
| Linux    | ✓         |
| Windows  | ✓         |
| macOS    | ✓         |
| Android  | x         |
| iOS      | x         |

## Install

_This plugin requires a Rust version of at least **1.95**._

There are three general methods of installation that we can recommend:

1. Use crates.io (easiest, and requires you to trust that our publishing pipeline worked)
2. Pull sources directly from GitHub using git tags / revision hashes (most secure)
3. Git submodule install this repo in your Tauri project and then use file protocol to ingest the source (most secure, but inconvenient to use)

Install the core plugin by adding the following to your `Cargo.toml` file:

`src-tauri/Cargo.toml`

```toml
# you can add the dependency in the `[dependencies]` section if you do not target mobile
[target."cfg(not(any(target_os = \"android\", target_os = \"ios\")))".dependencies]
tauri-plugin-updater-plus = "0.3"
# alternatively with Git:
tauri-plugin-updater-plus = { git = "https://github.com/0x8000/tauri-plugin-updater-plus", branch = "master" }
```

The TypeScript guest bindings (built on `@tauri-apps/api/core`) are in [`guest-js/index.ts`](guest-js/index.ts) and can be imported directly into your frontend project.

## Usage

First, register the core plugin with Tauri:

`src-tauri/src/lib.rs`

```rust
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater_plus::Builder::new().build())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Configure update endpoints in `src-tauri/tauri.conf.json`:

```json
{
  "plugins": {
    "updater-plus": {
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
  await relaunch()
}
```

## Generating Release Artifacts (`blockmap-gen`)

To publish differential updates, generate a `.blockmap` and manifest alongside your release files using `blockmap-gen`.

Download prebuilt binaries from [GitHub Releases](https://github.com/0x8000/tauri-plugin-updater-plus/releases), or install via Cargo:

```sh
cargo install tauri-plugin-updater-plus --no-default-features --features cli
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
