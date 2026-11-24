use super::{CachedPackageState, PackageCache};
use crate::Error;
use crate::blockmap::{BlockMap, BlockMapFile};
use crate::chunker::ChunkerConfig;

#[test]
fn commits_overwrites_and_preserves_source_when_seeding() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(dir.path().to_path_buf(), None);
    assert!(cache.load_base().is_none());

    let src_v1 = dir.path().join("v1.pkg");
    let v1_payload = vec![0x42u8; 64 * 1024];
    std::fs::write(&src_v1, &v1_payload).expect("write v1");
    cache.seed_from_file(&src_v1, "0.1.0").expect("seed v1");
    assert!(
        src_v1.is_file(),
        "seed_from_file must not delete caller source file"
    );

    let src_v2 = dir.path().join("v2.pkg");
    let v2_payload = vec![0x99u8; 70 * 1024];
    std::fs::write(&src_v2, &v2_payload).expect("write v2");
    cache.seed_from_file(&src_v2, "0.2.0").expect("seed v2");

    let base = cache.load_base().expect("loaded v2");
    assert_eq!(std::fs::read(&base.package_path).expect("read"), v2_payload);
    assert_eq!(cache.state().expect("state").version, "0.2.0");
    assert!(!dir.path().join("staging.pkg").exists());
    assert!(!dir.path().join("staging.blockmap").exists());
    assert!(!dir.path().join("staging.state.json").exists());
    assert!(!dir.path().join("seed.pkg").exists());
}

#[test]
fn leaves_previous_cache_intact_when_commit_blockmap_is_invalid() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(dir.path().to_path_buf(), None);

    let src_v1 = dir.path().join("v1.pkg");
    let v1_payload = vec![0x11u8; 48 * 1024];
    std::fs::write(&src_v1, &v1_payload).expect("write v1");
    cache.seed_from_file(&src_v1, "0.1.0").expect("seed v1");

    let src_v2 = dir.path().join("v2.pkg");
    std::fs::write(&src_v2, vec![0x22u8; 48 * 1024]).expect("write v2");
    let bad_map = BlockMap {
        version: "99".into(),
        files: vec![BlockMapFile {
            name: "file".into(),
            offset: 0,
            checksums: vec![],
            sizes: vec![],
        }],
    };
    let bad_state = CachedPackageState {
        version: "0.2.0".into(),
        size: 48 * 1024,
        sha512: None,
    };
    assert!(matches!(
        cache.commit(&src_v2, &bad_map, &bad_state),
        Err(Error::InvalidBlockMap(_))
    ));

    let base = cache.load_base().expect("v1 cache must remain valid");
    assert_eq!(std::fs::read(&base.package_path).expect("read"), v1_payload);
    assert_eq!(cache.state().expect("state").version, "0.1.0");
}

#[test]
fn falls_back_to_configured_appimage_when_cache_is_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    let appimage_path = dir.path().join("MyApp.AppImage");
    let payload = vec![0x55u8; 80 * 1024];
    std::fs::write(&appimage_path, &payload).expect("write appimage");

    let cache =
        PackageCache::with_appimage_fallback(dir.path().join("cache"), Some(appimage_path.clone()));
    let base = cache.load_base().expect("appimage fallback");
    assert_eq!(base.package_path, appimage_path);
    assert_eq!(base.blockmap.total_size(), payload.len() as u64);
}

#[test]
fn ignores_cache_when_package_size_or_blockmap_is_corrupted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(dir.path().to_path_buf(), None);

    let src = dir.path().join("v1.pkg");
    std::fs::write(&src, vec![7u8; 40_000]).expect("write");
    let artifact = BlockMap::generate(
        vec![7u8; 40_000].as_slice(),
        "file",
        ChunkerConfig::default(),
    )
    .expect("blockmap");
    let state = CachedPackageState {
        version: "0.1.0".into(),
        size: artifact.size,
        sha512: Some(artifact.sha512),
    };
    cache
        .commit(&src, &artifact.blockmap, &state)
        .expect("commit");

    std::fs::write(dir.path().join("current.pkg"), b"truncated").expect("corrupt pkg");
    assert!(cache.load_base().is_none());

    std::fs::write(dir.path().join("current.pkg"), vec![7u8; 40_000]).expect("restore pkg");
    std::fs::write(dir.path().join("current.blockmap"), b"not-valid-blockmap")
        .expect("corrupt map");
    assert!(cache.load_base().is_none());

    std::fs::write(dir.path().join("state.json"), b"{invalid json").expect("corrupt state");
    assert!(cache.state().is_none());
}
