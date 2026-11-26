use url::Url;

use super::ReleaseManifest;
use crate::Error;

#[test]
fn parses_tauri_static_json_with_blockmap_extensions_and_candidate_precedence() {
    let source = Url::parse("https://updates.example.com/latest.json").expect("url");
    let json = r#"{
        "version": "v0.2.0",
        "notes": "Delta update test",
        "pub_date": "2026-11-24T00:00:00Z",
        "platforms": {
            "linux-x86_64-appimage": {
                "url": "https://updates.example.com/myapp_0.2.0.AppImage",
                "signature": "sig-appimage==",
                "sha512": "hash-appimage==",
                "size": 102400,
                "blockMapUrl": "https://cdn.example.com/custom.blockmap",
                "blockmap_size": 1024
            },
            "linux-x86_64": {
                "url": "https://updates.example.com/myapp_0.2.0.tar.gz",
                "signature": "sig-tar==",
                "sha512": "hash-tar==",
                "size": 99999
            }
        }
    }"#;

    let manifest = ReleaseManifest::parse(json, &source).expect("parse json");
    assert_eq!(manifest.version.to_string(), "0.2.0");

    let preferred = manifest
        .resolve_platform(&["linux-x86_64-appimage".into(), "linux-x86_64".into()])
        .expect("resolve preferred");
    assert_eq!(preferred.size, Some(102400));
    assert_eq!(
        preferred
            .resolved_blockmap_url()
            .expect("explicit blockmap url")
            .as_str(),
        "https://cdn.example.com/custom.blockmap"
    );

    let fallback = manifest
        .resolve_platform(&["linux-x86_64-deb".into(), "linux-x86_64".into()])
        .expect("resolve fallback");
    assert_eq!(
        fallback
            .resolved_blockmap_url()
            .expect("derived blockmap url")
            .as_str(),
        "https://updates.example.com/myapp_0.2.0.tar.gz.blockmap"
    );

    // Derived `.blockmap` URLs must strip presigned query strings and fragments.
    let presigned = super::ReleasePlatform {
        url: Url::parse("https://bucket.s3.amazonaws.com/app.exe?X-Amz-Signature=deadbeef#frag")
            .expect("url"),
        signature: None,
        sha512: Some("hash==".into()),
        size: Some(1234),
        blockmap_url: None,
        blockmap_size: None,
    };
    assert_eq!(
        presigned
            .resolved_blockmap_url()
            .expect("derived presigned blockmap url")
            .as_str(),
        "https://bucket.s3.amazonaws.com/app.exe.blockmap"
    );

    let roundtrip_json = manifest.to_tauri_json().expect("serialize tauri json");
    let reparsed = ReleaseManifest::parse(&roundtrip_json, &source).expect("reparse json");
    assert_eq!(reparsed.version, manifest.version);
    assert_eq!(reparsed.notes, manifest.notes);
    assert_eq!(reparsed.pub_date, manifest.pub_date);
    assert_eq!(reparsed.platforms, manifest.platforms);

    let map_yaml = manifest
        .to_electron_yaml(&["linux-x86_64-appimage".into()])
        .expect("serialize map target to electron yaml");
    assert!(map_yaml.contains("url: myapp_0.2.0.AppImage"));
    assert!(map_yaml.contains("blockMapUrl: 'https://cdn.example.com/custom.blockmap'"));
    assert!(matches!(
        manifest.to_electron_yaml(&["windows-x86_64".into()]),
        Err(Error::TargetNotFound(ref target)) if target.contains("windows-x86_64")
    ));
}

#[test]
fn parses_electron_latest_linux_yml_and_resolves_relative_urls() {
    let source = Url::parse("https://cdn.example.com/releases/latest-linux.yml").expect("url");
    let yaml = "\
version: 1.4.2
files:
  - url: MyApp-1.4.2.AppImage
    sha512: ZjA+AWAcl9e8xqU1==
    size: 134217728
    blockMapSize: 65432
path: MyApp-1.4.2.AppImage
sha512: ZjA+AWAcl9e8xqU1==
releaseDate: '2026-11-24T00:00:00.000Z'
releaseNotes: 'Reduced update download size'
";

    let manifest = ReleaseManifest::parse(yaml, &source).expect("parse electron yaml");
    assert_eq!(manifest.version.to_string(), "1.4.2");
    assert_eq!(
        manifest.notes.as_deref(),
        Some("Reduced update download size")
    );
    assert_eq!(
        manifest.formatted_pub_date().as_deref(),
        Some("2026-11-24T00:00:00Z")
    );
    let single_json = manifest.to_tauri_json().expect("serialize single json");
    let reparsed_single = ReleaseManifest::parse(&single_json, &source).expect("reparse single");
    assert_eq!(reparsed_single.platforms, manifest.platforms);
    let platform = manifest
        .resolve_platform(&["linux-x86_64".into()])
        .expect("single platform");
    assert_eq!(
        platform.url.as_str(),
        "https://cdn.example.com/releases/MyApp-1.4.2.AppImage"
    );
    assert_eq!(platform.sha512.as_deref(), Some("ZjA+AWAcl9e8xqU1=="));
    assert_eq!(platform.size, Some(134217728));
    assert_eq!(platform.blockmap_size, Some(65432));
    assert_eq!(
        platform
            .resolved_blockmap_url()
            .expect("blockmap url")
            .as_str(),
        "https://cdn.example.com/releases/MyApp-1.4.2.AppImage.blockmap"
    );
}

#[test]
fn parses_electron_yaml_top_level_path_fallback_without_files_list() {
    let source = Url::parse("https://cdn.example.com/releases/latest.yml").expect("url");
    let yaml = "\
version: 2.0.0
path: MyApp-Setup-2.0.0.exe
sha512: topHash==
releaseDate: '2026-11-24T00:00:00Z'
";

    let manifest = ReleaseManifest::parse(yaml, &source).expect("parse top-level yaml");
    let platform = manifest
        .resolve_platform(&["windows-x86_64".into()])
        .expect("platform");
    assert_eq!(
        platform.url.as_str(),
        "https://cdn.example.com/releases/MyApp-Setup-2.0.0.exe"
    );
    assert_eq!(platform.sha512.as_deref(), Some("topHash=="));
}

#[test]
fn roundtrips_generated_electron_yaml_including_all_artifact_fields() {
    let source = Url::parse("https://cdn.example.com/latest-linux.yml").expect("url");
    let json = r#"{
        "version": "0.3.0",
        "notes": "Fix Bob's edge case\nversion: 9.9.9\nKeep \"quotes\" and \\slashes\\",
        "pub_date": "2026-11-24T00:00:00Z",
        "url": "https://cdn.example.com/myapp-0.3.0.AppImage",
        "sha512": "abc123==",
        "size": 50000,
        "blockmap_url": "https://cdn.example.com/custom/myapp-0.3.0.blockmap",
        "blockmap_size": 1200
    }"#;
    let manifest = ReleaseManifest::parse(json, &source).expect("parse json");
    let yaml = manifest
        .to_electron_yaml(&["linux-x86_64".into()])
        .expect("serialize yaml");
    let reparsed = ReleaseManifest::parse(&yaml, &source).expect("reparse yaml");

    assert_eq!(reparsed.version, manifest.version);
    assert_eq!(reparsed.notes, manifest.notes);
    assert_eq!(reparsed.pub_date, manifest.pub_date);
    let orig = manifest
        .resolve_platform(&["linux-x86_64".into()])
        .expect("orig");
    let next = reparsed
        .resolve_platform(&["linux-x86_64".into()])
        .expect("reparsed");
    assert_eq!(next.url, orig.url);
    assert_eq!(next.sha512, orig.sha512);
    assert_eq!(next.size, orig.size);
    assert_eq!(next.blockmap_url, orig.blockmap_url);
    assert_eq!(next.blockmap_size, orig.blockmap_size);

    // Block scalar `|-` emitted by electron-builder must also preserve multiline notes cleanly.
    let block_yaml = "\
version: 0.4.0
path: myapp-0.4.0.AppImage
sha512: hash040==
releaseNotes: |-
  First bullet
  version: 0.0.1
releaseDate: '2026-11-24T00:00:00Z'
";
    let block_parsed = ReleaseManifest::parse(block_yaml, &source).expect("parse block scalar");
    assert_eq!(block_parsed.version.to_string(), "0.4.0");
    assert_eq!(
        block_parsed.notes.as_deref(),
        Some("First bullet\nversion: 0.0.1")
    );
}

#[test]
fn rejects_invalid_manifests_and_missing_targets_with_typed_errors() {
    let source = Url::parse("https://cdn.example.com/latest.json").expect("url");

    let missing_url = r#"{"version":"1.0.0"}"#;
    assert!(matches!(
        ReleaseManifest::parse(missing_url, &source),
        Err(Error::InvalidManifest(ref msg)) if msg.contains("missing `url`")
    ));

    let bad_date = r#"{"version":"1.0.0","pub_date":"not-a-date","url":"https://a.test/x"}"#;
    assert!(matches!(
        ReleaseManifest::parse(bad_date, &source),
        Err(Error::InvalidManifest(ref msg)) if msg.contains("pub_date")
    ));

    let bad_semver = r#"{"version":"not-semver","url":"https://a.test/x"}"#;
    assert!(matches!(
        ReleaseManifest::parse(bad_semver, &source),
        Err(Error::Semver(_))
    ));

    let static_json =
        r#"{"version":"1.0.0","platforms":{"darwin-aarch64":{"url":"https://a.test/m.zip"}}}"#;
    let manifest = ReleaseManifest::parse(static_json, &source).expect("parse static");
    assert!(matches!(
        manifest.resolve_platform(&["linux-x86_64".into()]),
        Err(Error::TargetNotFound(ref target)) if target.contains("linux-x86_64")
    ));

    assert!(matches!(
        ReleaseManifest::parse("path: app.AppImage\n", &source),
        Err(Error::InvalidManifest(ref msg)) if msg.contains("missing `version`")
    ));
    assert!(matches!(
        ReleaseManifest::parse("version: 1.0.0\n", &source),
        Err(Error::InvalidManifest(ref msg)) if msg.contains("missing `files[0].url`")
    ));
}
