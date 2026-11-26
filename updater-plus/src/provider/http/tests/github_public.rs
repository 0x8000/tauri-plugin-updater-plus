use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use semver::Version;
use url::Url;

use super::super::GitHubProvider;
use super::{serve_single_range_or_full, synthetic_v1_v2};
use crate::Error;
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadMode, DummyListener};
use crate::updater::UpdaterBuilder;

#[tokio::test]
async fn github_provider_supports_public_direct_prerelease_filtering_and_does_not_leak_token_to_external_host()
 {
    let (v1, v2) = synthetic_v1_v2();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let bm_bytes = Arc::new(v2_art.blockmap.to_gzip_bytes().expect("gz"));

    // External host that rejects any GitHub token or API header even on hop 0.
    #[derive(Clone)]
    struct ExternalState {
        artifact: Arc<Vec<u8>>,
        blockmap: Arc<Vec<u8>>,
        leaked: Arc<AtomicUsize>,
    }
    async fn serve_external(
        State(state): State<ExternalState>,
        AxumPath(name): AxumPath<String>,
        headers: HeaderMap,
    ) -> Response {
        if headers.contains_key(AUTHORIZATION) || headers.contains_key("x-github-api-version") {
            state.leaked.fetch_add(1, Ordering::SeqCst);
            return (
                StatusCode::BAD_REQUEST,
                "github token leaked to external host",
            )
                .into_response();
        }
        match name.as_str() {
            "external.AppImage" => serve_single_range_or_full(&state.artifact, &headers),
            "external.AppImage.blockmap" => {
                (StatusCode::OK, (*state.blockmap).clone()).into_response()
            }
            _ => (StatusCode::NOT_FOUND, "missing").into_response(),
        }
    }

    let ext_state = ExternalState {
        artifact: Arc::new(v2.clone()),
        blockmap: Arc::clone(&bm_bytes),
        leaked: Arc::new(AtomicUsize::new(0)),
    };
    let ext_check = ext_state.clone();
    let ext_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ext");
    let ext_base = Url::parse(&format!(
        "http://{}/",
        ext_listener.local_addr().expect("addr")
    ))
    .expect("ext url");
    tokio::spawn(async move {
        let app = Router::new()
            .route("/files/{name}", get(serve_external))
            .with_state(ext_state);
        let _ = axum::serve(ext_listener, app).await;
    });

    // Public direct web_base_url server.
    let public_manifest = serde_json::to_vec(&serde_json::json!({
        "version": "0.2.0",
        "platforms": {
            "linux-x86_64": {
                "url": ext_base.join("files/external.AppImage").expect("url"),
                "sha512": v2_art.sha512,
                "size": v2_art.size
            }
        }
    }))
    .expect("json");
    let public_manifest_arc = Arc::new(public_manifest);
    let pub_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind pub");
    let pub_base = Url::parse(&format!(
        "http://{}/",
        pub_listener.local_addr().expect("addr")
    ))
    .expect("pub url");
    let manifest_for_pub = Arc::clone(&public_manifest_arc);
    tokio::spawn(async move {
        let app = Router::new().route(
            "/acme/app/releases/latest/download/latest.json",
            get(move || {
                let bytes = Arc::clone(&manifest_for_pub);
                async move { (StatusCode::OK, (*bytes).clone()) }
            }),
        );
        let _ = axum::serve(pub_listener, app).await;
    });

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let v1_path = temp.path().join("v1.pkg");
    std::fs::write(&v1_path, &v1).expect("write v1");
    cache.seed_from_file(&v1_path, "0.1.0").expect("seed");

    let public_provider = Arc::new(
        GitHubProvider::builder("acme", "app")
            .web_base_url(pub_base)
            .build()
            .expect("public provider"),
    );
    let pub_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .target("linux-x86_64")
    .provider(public_provider)
    .build()
    .expect("pub updater");
    let pub_update = pub_updater
        .check()
        .await
        .expect("pub check")
        .expect("update");
    assert_eq!(pub_update.version, "0.2.0");

    // Private API with include_prerelease skipping drafts and pointing manifest URL to external
    // host (unmatched filename in release assets -> never leaks GitHub token to external host).
    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind api");
    let api_base = Url::parse(&format!(
        "http://{}/",
        api_listener.local_addr().expect("addr")
    ))
    .expect("api url");
    let api_base_for_srv = api_base.clone();
    let manifest_for_api = Arc::clone(&public_manifest_arc);
    tokio::spawn(async move {
        let app = Router::new()
            .route(
                "/repos/acme/app/releases",
                get(move || {
                    let api = api_base_for_srv.clone();
                    async move {
                        let body = serde_json::json!([
                            { "tag_name": "v0.3.0-draft", "draft": true, "assets": [] },
                            {
                                "tag_name": "v0.2.0-rc.1",
                                "draft": false,
                                "prerelease": true,
                                "assets": [
                                    {
                                        "name": "latest.json",
                                        "url": api
                                            .join("repos/acme/app/releases/assets/1")
                                            .expect("u")
                                            .as_str()
                                    }
                                ]
                            }
                        ]);
                        (StatusCode::OK, serde_json::to_vec(&body).expect("json"))
                    }
                }),
            )
            .route(
                "/repos/acme/app/releases/assets/1",
                get(move || {
                    let bytes = Arc::clone(&manifest_for_api);
                    async move { (StatusCode::OK, (*bytes).clone()) }
                }),
            )
            .route(
                "/repos/acme/drafts-only/releases",
                get(|| async {
                    (
                        StatusCode::OK,
                        serde_json::to_vec(&serde_json::json!([
                            { "tag_name": "v1.0.0-draft", "draft": true, "assets": [] }
                        ]))
                        .expect("json"),
                    )
                }),
            )
            .route(
                "/repos/acme/bad-json/releases",
                get(|| async { (StatusCode::OK, b"{}".to_vec()) }),
            );
        let _ = axum::serve(api_listener, app).await;
    });

    let pre_provider = Arc::new(
        GitHubProvider::builder("acme", "app")
            .token("ghp_private_secret")
            .include_prerelease(true)
            .api_base_url(api_base.clone())
            .build()
            .expect("pre provider"),
    );
    let pre_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .target("linux-x86_64")
    .provider(pre_provider)
    .build()
    .expect("pre updater");
    let pre_update = pre_updater
        .check()
        .await
        .expect("pre check")
        .expect("update");
    let pre_pkg = pre_update.download(&DummyListener).await.expect("download");
    assert_eq!(pre_pkg.summary.mode, DownloadMode::Differential);
    assert_eq!(ext_check.leaked.load(Ordering::SeqCst), 0);

    // All-draft releases returns Ok(None).
    let drafts_provider = Arc::new(
        GitHubProvider::builder("acme", "drafts-only")
            .include_prerelease(true)
            .api_base_url(api_base.clone())
            .build()
            .expect("drafts provider"),
    );
    let drafts_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache.clone(),
    )
    .provider(drafts_provider)
    .build()
    .expect("drafts updater");
    assert!(drafts_updater.check().await.expect("all drafts").is_none());

    // Non-array JSON for prerelease list returns InvalidManifest.
    let bad_json_provider = Arc::new(
        GitHubProvider::builder("acme", "bad-json")
            .include_prerelease(true)
            .api_base_url(api_base)
            .build()
            .expect("bad json provider"),
    );
    let bad_json_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache,
    )
    .provider(bad_json_provider)
    .build()
    .expect("bad json updater");
    assert!(matches!(
        bad_json_updater.check().await,
        Err(Error::InvalidManifest(ref msg)) if msg.contains("not a JSON array")
    ));
}
