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

Add to `src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-updater-plus = "0.4"
```

The TypeScript guest bindings (built on `@tauri-apps/api/core`) are in
[`guest-js/index.ts`](guest-js/index.ts).

## Usage

Register the plugin with Tauri in `src-tauri/src/lib.rs`:

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

You can also replace the default OS `PlatformInstaller` with a custom
`UpdateInstaller` via `Builder::new().installer(...)`.

## License

Licensed under the [Apache License, Version 2.0](../LICENSE).
