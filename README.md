# tauri-plugin-updater-plus

Electron-style blockmap differential updater plugin and CLI for Tauri v3 applications.

## How It Works

1. **Content-Defined Chunking (FastCDC)**: `blockmap-gen` splits release
   artifacts into content-defined chunks (default 16 KiB min / 64 KiB avg / 256
   KiB max), hashes each chunk with SHA-256 (`aws-lc-rs`), and emits a
   gzip-compressed Electron v2 `.blockmap` file alongside the release manifest.
2. **Manifest Compatibility**: Reads both Tauri `latest.json` (with optional
   `sha512`, `size`, `blockmap_url`, and `blockmap_size` fields plus minisign
   signatures) and Electron `latest*.yml` (`latest.yml`, `latest-linux.yml`,
   `latest-mac.yml`).
3. **Differential Download & Reconstruction**: Compares the locally cached base
   package (or the running `$APPIMAGE` on Linux) against the remote `.blockmap`,
   copies and SHA-256-verifies unchanged local blocks, fetches only changed byte
   ranges via HTTP `Range`, and verifies the reconstructed package with SHA-512,
   optional streaming minisign signatures, and native OS code signing (macOS
   `codesign` Designated Requirement / Windows Authenticode) before caching and
   installing it.

## Installation

Add the plugin to your Tauri app's `src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-updater-plus = "0.2"
```

Register the plugin in `src-tauri/src/lib.rs`:

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_updater_plus::Builder::new().build())
```

Configure endpoints in `src-tauri/tauri.conf.json`:

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

## Hosting Support

- **Static File Servers (Nginx / Caddy / Apache)**: Upload the manifest
  (`latest.json` or `latest*.yml`), package artifact, and `<artifact>.blockmap`
  in the same directory. Multi-range `multipart/byteranges` is used
  automatically when supported, degrading cleanly to concurrent single-range or
  full download.
- **GitHub Releases & Object Stores (S3 / Cloudflare R2 / OSS / Azure Blob /
  GCS)**: Upload the manifest, artifact, and `<artifact>.blockmap` as release
  assets or bucket objects. Known single-range hosts automatically skip
  multi-range probes and use concurrent single-range HTTP `Range` requests;
  servers returning `200 OK` on multi-range requests automatically degrade to
  single-range `206` before falling back to full download. Resolved `302` CDN
  URLs (e.g. `objects.githubusercontent.com`) are reused across range requests,
  re-resolved transparently if a signed token expires, and cross-origin
  `Authorization`/`Cookie` headers are stripped.

## CLI Usage (`blockmap-gen`)

Download prebuilt binaries from [GitHub Releases](https://github.com/0x8000/tauri-plugin-updater-plus/releases),
or install from source without Tauri dependencies:

```bash
cargo install tauri-plugin-updater-plus --no-default-features --features cli
```

```bash
# Generate an Electron v2 .blockmap for an artifact
blockmap-gen generate --input MyApp.AppImage

# Generate a .blockmap and Tauri/Electron release manifest
blockmap-gen manifest \
  --input MyApp-0.2.0.AppImage \
  --version 0.2.0 \
  --url https://example.com/MyApp-0.2.0.AppImage \
  --format tauri \
  --target linux-x86_64 \
  --output latest.json

# Measure differential savings between two blockmaps
blockmap-gen diff \
  --old-blockmap v1.blockmap \
  --new-blockmap v2.blockmap

# Pack a directory into a per-entry deflated ZIP friendly to blockmap updates
blockmap-gen pack-zip --dir MyApp.app --output MyApp.app.zip
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
