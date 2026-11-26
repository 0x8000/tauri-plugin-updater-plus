use axum::http::StatusCode;
use semver::Version;
use url::Url;

use super::super::{TargetResolver, UpdaterBuilder};
use super::{ReleaseServerState, spawn_release_server};
use crate::Error;
use crate::cache::PackageCache;
use crate::config::Config;

#[tokio::test]
async fn handles_version_comparisons_204_failover_and_builder_validation() {
    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);

    assert!(matches!(
        UpdaterBuilder::new(
            "myapp".into(),
            Version::new(1, 0, 0),
            temp.path().join("app"),
            cache.clone(),
        )
        .build(),
        Err(Error::InvalidConfig(_))
    ));
    assert!(matches!(
        UpdaterBuilder::new(
            "myapp".into(),
            Version::new(1, 0, 0),
            temp.path().join("app"),
            cache.clone(),
        )
        .header("bad header name", "val"),
        Err(Error::InvalidHeader(_))
    ));

    let state = ReleaseServerState::default();
    let base_url = spawn_release_server(state.clone()).await;
    {
        state
            .status_overrides
            .lock()
            .expect("lock")
            .insert("no-update".into(), StatusCode::NO_CONTENT);
        state
            .status_overrides
            .lock()
            .expect("lock")
            .insert("failing".into(), StatusCode::INTERNAL_SERVER_ERROR);
        let manifest = serde_json::json!({
            "version": "0.9.0",
            "url": base_url.join("pkg").expect("url"),
            "sha512": "dummyHash==",
        });
        state.files.lock().expect("lock").insert(
            "v09.json".into(),
            serde_json::to_vec(&manifest).expect("json"),
        );
    }

    let no_content_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(1, 0, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .endpoints(vec![base_url.join("no-update").expect("url")])
    .build()
    .expect("updater");
    assert!(no_content_updater.check().await.expect("204 ok").is_none());

    let downgrade_blocked = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(1, 0, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .endpoints(vec![
        base_url.join("failing").expect("url"),
        base_url.join("v09.json").expect("url"),
    ])
    .build()
    .expect("updater");
    assert!(
        downgrade_blocked
            .check()
            .await
            .expect("failover ok")
            .is_none()
    );

    let mut allow_config = Config {
        allow_downgrades: true,
        ..Config::default()
    };
    allow_config.endpoints = vec![
        base_url.join("failing").expect("url"),
        base_url.join("v09.json").expect("url"),
    ];
    let downgrade_allowed = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(1, 0, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .config(allow_config.clone())
    .build()
    .expect("updater");
    let update = downgrade_allowed
        .check()
        .await
        .expect("downgrade allowed")
        .expect("update");
    assert_eq!(update.version, "0.9.0");

    let same_ver_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 9, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .config(allow_config)
    .build()
    .expect("same version updater");
    assert!(
        same_ver_updater
            .check()
            .await
            .expect("equal version check")
            .is_none(),
        "equal version must not reinstall even with allow_downgrades=true"
    );

    for bad_threshold in [-0.1, 1.5, f64::NAN] {
        let bad_cfg = Config {
            endpoints: vec![base_url.join("v09.json").expect("url")],
            full_download_threshold: bad_threshold,
            ..Config::default()
        };
        assert!(matches!(
            UpdaterBuilder::new(
                "myapp".into(),
                Version::new(0, 1, 0),
                temp.path().join("app"),
                cache.clone()
            )
            .config(bad_cfg)
            .build(),
            Err(Error::InvalidConfig(ref msg)) if msg.contains("fullDownloadThreshold")
        ));
    }
    let blank_pubkey_cfg = Config {
        endpoints: vec![base_url.join("v09.json").expect("url")],
        pubkey: Some("   ".into()),
        ..Config::default()
    };
    assert!(matches!(
        UpdaterBuilder::new(
            "myapp".into(),
            Version::new(0, 1, 0),
            temp.path().join("app"),
            cache.clone()
        )
        .config(blank_pubkey_cfg)
        .build(),
        Err(Error::InvalidConfig(ref msg)) if msg.contains("pubkey")
    ));
    let bad_dn_cfg = Config {
        endpoints: vec![base_url.join("v09.json").expect("url")],
        windows: Some(crate::config::WindowsConfig {
            publisher_name: vec!["NotADistinguishedName".into()],
            ..Default::default()
        }),
        ..Config::default()
    };
    assert!(matches!(
        UpdaterBuilder::new(
            "myapp".into(),
            Version::new(0, 1, 0),
            temp.path().join("app"),
            cache.clone()
        )
        .config(bad_dn_cfg)
        .build(),
        Err(Error::InvalidConfig(ref msg)) if msg.contains("publisherName")
    ));
    assert!(matches!(
        UpdaterBuilder::new(
            "myapp".into(),
            Version::new(1, 0, 0),
            temp.path().join("app"),
            cache.clone(),
        )
        .header("x-token", "bad\r\nheader"),
        Err(Error::InvalidHeader(_))
    ));

    {
        let unverified = serde_json::json!({
            "version": "2.0.0",
            "url": base_url.join("pkg").expect("url"),
        });
        let wrong_target = serde_json::json!({
            "version": "2.0.0",
            "platforms": {
                "darwin-aarch64": {
                    "url": base_url.join("pkg").expect("url"),
                    "sha512": "validHash=="
                }
            }
        });
        let valid_mirror = serde_json::json!({
            "version": "2.0.0",
            "url": base_url.join("pkg").expect("url"),
            "sha512": "validHash==",
        });
        let mut files = state.files.lock().expect("lock");
        files.insert(
            "unverified.json".into(),
            serde_json::to_vec(&unverified).expect("json"),
        );
        files.insert(
            "wrong-target.json".into(),
            serde_json::to_vec(&wrong_target).expect("json"),
        );
        files.insert(
            "valid-mirror.json".into(),
            serde_json::to_vec(&valid_mirror).expect("json"),
        );
    }
    let unverified_only = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(1, 0, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .endpoints(vec![base_url.join("unverified.json").expect("url")])
    .build()
    .expect("updater");
    assert!(matches!(
        unverified_only.check().await,
        Err(Error::InvalidManifest(ref msg)) if msg.contains("sha512")
    ));

    let mirror_failover = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(1, 0, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .target("linux-x86_64")
    .endpoints(vec![
        base_url.join("wrong-target.json").expect("url"),
        base_url.join("unverified.json").expect("url"),
        base_url.join("valid-mirror.json").expect("url"),
    ])
    .build()
    .expect("updater");
    assert_eq!(
        mirror_failover
            .check()
            .await
            .expect("failover to valid mirror")
            .expect("update")
            .version,
        "2.0.0"
    );
}

#[test]
fn expands_endpoint_template_placeholders() {
    let raw = Url::parse(
        "https://updates.example.com/{{target}}/{{current_version}}/latest.json?t={{target}}&a={{arch}}&b={{bundle_type}}&v={{current_version}}",
    )
    .expect("url");
    let expanded = TargetResolver::expand_endpoint_with_parts(
        &raw,
        "linux-x86_64",
        "x86_64",
        "appimage",
        "0.1.0+build 1#rc?2",
    )
    .expect("expand");
    assert_eq!(
        expanded.as_str(),
        "https://updates.example.com/linux-x86_64/0.1.0+build%201%23rc%3F2/latest.json?t=linux-x86_64&a=x86_64&b=appimage&v=0.1.0+build%201%23rc%3F2"
    );
}
