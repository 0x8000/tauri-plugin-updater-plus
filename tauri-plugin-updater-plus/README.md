# tauri-plugin-updater-plus

[![crates.io](https://img.shields.io/crates/v/tauri-plugin-updater-plus.svg)](https://crates.io/crates/tauri-plugin-updater-plus)
[![docs.rs](https://docs.rs/tauri-plugin-updater-plus/badge.svg)](https://docs.rs/tauri-plugin-updater-plus)

Differential in-app updates for Tauri v3 applications, powered by
[`updater-plus`](../updater-plus).

| Platform | Supported |
| -------- | --------- |
| Linux    | ✓         |
| Windows  | ✓         |
| macOS    | ✓         |
| Android  | ✓         |
| iOS      | x         |

## Install

_This plugin requires a Rust version of at least **1.95**._

Add `tauri-plugin-updater-plus` to `src-tauri/Cargo.toml`:

```toml
# add to `[dependencies]` to target Linux, Windows, macOS, and Android
[dependencies]
tauri-plugin-updater-plus = "0.5"
# alternatively with Git:
tauri-plugin-updater-plus = { git = "https://github.com/0x8000/updater-plus", branch = "master" }
```

The TypeScript guest bindings (built on `@tauri-apps/api/core`) are in
[`guest-js/index.ts`](guest-js/index.ts) and can be imported directly into your
frontend project.

## Usage

First, register the plugin with Tauri in `src-tauri/src/lib.rs`:

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

You can also plug in a custom `UpdateProvider` or replace the default OS
`PlatformInstaller` facade with a custom `UpdateInstaller` via
`Builder::new().provider(...)` / `Builder::new().installer(...)`.

Configure update endpoints in `src-tauri/tauri.conf.json`. Use `plugins.updater`
when `bundle.createUpdaterArtifacts` is enabled so Tauri CLI's updater bundling
and `tauri-plugin-updater-plus` share a single configuration
(`plugins.updater-plus` is also supported standalone or as an overlay on top of
`plugins.updater`):

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

Afterwards, all plugin APIs are available through the JavaScript/TypeScript
guest bindings:

```ts
import { check } from "./guest-js";
import { relaunch } from "@tauri-apps/plugin-process";

const update = await check();
if (update) {
  await update.downloadAndInstall();
  // Relaunch the app on macOS and Linux to run the newly installed version
  // (Android hands off to the system package installer instead of relaunching).
  await relaunch();
}
```

On Android, the plugin resolves `android-{arch}-apk`, `android-{arch}`,
`android-universal`, and `android` manifest targets, uses the installed base APK
as a differential blockmap cache fallback, and hands verified `.apk` packages to
the system package installer via a dedicated `FileProvider` (requesting
`REQUEST_INSTALL_PACKAGES` on Android 8+ when needed).

## Generating Release Artifacts (`blockmap-gen`)

See the root [README](../README.md#generating-release-artifacts-blockmap-gen)
for generating `.blockmap` files and manifests with `blockmap-gen`.

## License

Licensed under the [Apache License, Version 2.0](../LICENSE).
