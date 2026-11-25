# updater-plus

[![crates.io](https://img.shields.io/crates/v/updater-plus.svg)](https://crates.io/crates/updater-plus)
[![docs.rs](https://docs.rs/updater-plus/badge.svg)](https://docs.rs/updater-plus)

Framework-agnostic Electron-style blockmap differential update engine and `blockmap-gen` CLI.

## Features

- `default = ["http", "native-installer"]`: full HTTP/GitHub/S3 differential engine plus OS-native installers (`DesktopInstaller`, `AndroidInstaller`, and `PlatformInstaller` facade).
- `engine`: transport-agnostic differential core (`FastCdc`, `BlockMap`, `DownloadPlan`, `DifferentialDownloader`, `PackageCache`, `ArtifactVerifier`, `UpdateProvider`, `UpdateInstaller`, `Updater`).
- `http`: built-in HTTP, GitHub Releases, and S3 providers on top of `engine` (no OS installer dependencies).
- `native-installer`: built-in `DesktopInstaller` (Linux/macOS/Windows), `AndroidInstaller`, and `PlatformInstaller` OS facade.
- `cli`: builds the `blockmap-gen` CLI binary.

## Usage

Use only the differential download engine (for custom hot-update bundles such as web assets or Lua scripts):

```toml
[dependencies]
updater-plus = { version = "0.4", default-features = false, features = ["http"] }
```

Use with built-in OS installers or implement `UpdateInstaller` to plug in your own deployment strategy:

```rust
use std::sync::Arc;
use updater_plus::{PlatformInstaller, UpdaterBuilder};

// Override default OS installation with any custom `UpdateInstaller`,
// or use `PlatformInstaller::current()` / `DesktopInstaller` / `AndroidInstaller`.
let _ = PlatformInstaller::current();
```

Install the `blockmap-gen` CLI:

```sh
cargo install updater-plus --no-default-features --features cli
```

## License

Licensed under the [Apache License, Version 2.0](../LICENSE).
