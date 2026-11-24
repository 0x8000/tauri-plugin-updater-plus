use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{ACCEPT, AUTHORIZATION};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
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
async fn github_provider_resolves_private_release_assets_and_validates_config() {
    assert!(matches!(
        GitHubProvider::builder("", "repo").build(),
        Err(Error::InvalidConfig(_))
    ));
    assert!(matches!(
        GitHubProvider::builder("owner", "  ").build(),
        Err(Error::InvalidConfig(_))
    ));

    let (v1, v2) = synthetic_v1_v2();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let bm_bytes = v2_art.blockmap.to_gzip_bytes().expect("gz");

    #[derive(Clone)]
    struct GhCdnState {
        assets: Arc<HashMap<String, Vec<u8>>>,
        leaked: Arc<AtomicUsize>,
    }
    async fn serve_gh_cdn(
        State(state): State<GhCdnState>,
        AxumPath(name): AxumPath<String>,
        headers: HeaderMap,
    ) -> Response {
        if headers.contains_key(AUTHORIZATION)
            || headers.contains_key("x-github-api-version")
            || headers.get(ACCEPT).and_then(|v| v.to_str().ok()) == Some("application/octet-stream")
        {
            state.leaked.fetch_add(1, Ordering::SeqCst);
            return (StatusCode::BAD_REQUEST, "github API headers leaked to CDN").into_response();
        }
        let Some(data) = state.assets.get(&name) else {
            return (StatusCode::NOT_FOUND, "missing").into_response();
        };
        serve_single_range_or_full(data, &headers)
    }

    let mut cdn_assets = HashMap::new();
    cdn_assets.insert("MyApp.AppImage".to_string(), v2.clone());
    cdn_assets.insert("MyApp.AppImage.blockmap".to_string(), bm_bytes);
    let gh_cdn_state = GhCdnState {
        assets: Arc::new(cdn_assets),
        leaked: Arc::new(AtomicUsize::new(0)),
    };
    let cdn_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cdn");
    let cdn_base = Url::parse(&format!(
        "http://{}/",
        cdn_listener.local_addr().expect("addr")
    ))
    .expect("cdn url");
    let gh_cdn_check = gh_cdn_state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/cdn/{name}", get(serve_gh_cdn))
            .with_state(gh_cdn_state);
        let _ = axum::serve(cdn_listener, app).await;
    });

    #[derive(Clone)]
    struct GhApiState {
        api_base: Url,
        cdn_base: Url,
        manifest_bytes: Arc<Vec<u8>>,
    }

    async fn serve_gh_latest(State(state): State<GhApiState>, headers: HeaderMap) -> Response {
        if headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer ghp_test_token")
        {
            return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        }
        let body = serde_json::json!({
            "tag_name": "v0.2.0",
            "draft": false,
            "prerelease": false,
            "assets": [
                {
                    "name": "latest.json",
                    "url": state.api_base.join("repos/acme/app/releases/assets/101").expect("url").as_str()
                },
                {
                    "name": "MyApp.AppImage",
                    "url": state.api_base.join("repos/acme/app/releases/assets/102").expect("url").as_str()
                },
                {
                    "name": "MyApp.AppImage.blockmap",
                    "url": state.api_base.join("repos/acme/app/releases/assets/103").expect("url").as_str()
                }
            ]
        });
        (StatusCode::OK, serde_json::to_vec(&body).expect("json")).into_response()
    }

    async fn serve_gh_asset(
        State(state): State<GhApiState>,
        AxumPath(id): AxumPath<u64>,
        headers: HeaderMap,
    ) -> Response {
        if headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer ghp_test_token")
        {
            return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        }
        if headers.get(ACCEPT).and_then(|v| v.to_str().ok()) != Some("application/octet-stream") {
            return (StatusCode::BAD_REQUEST, "missing octet-stream Accept").into_response();
        }
        match id {
            101 => (StatusCode::OK, (*state.manifest_bytes).clone()).into_response(),
            102 => Redirect::temporary(
                state
                    .cdn_base
                    .join("cdn/MyApp.AppImage")
                    .expect("url")
                    .as_str(),
            )
            .into_response(),
            103 => Redirect::temporary(
                state
                    .cdn_base
                    .join("cdn/MyApp.AppImage.blockmap")
                    .expect("url")
                    .as_str(),
            )
            .into_response(),
            _ => (StatusCode::NOT_FOUND, "unknown asset").into_response(),
        }
    }

    let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind api");
    let api_base = Url::parse(&format!(
        "http://{}/",
        api_listener.local_addr().expect("addr")
    ))
    .expect("api url");
    let manifest_bytes = serde_json::to_vec(&serde_json::json!({
        "version": "0.2.0",
        "platforms": {
            "linux-x86_64": {
                "url": "https://github.com/acme/app/releases/download/v0.2.0/MyApp.AppImage",
                "sha512": v2_art.sha512,
                "size": v2_art.size
            }
        }
    }))
    .expect("json");
    let api_state = GhApiState {
        api_base: api_base.clone(),
        cdn_base,
        manifest_bytes: Arc::new(manifest_bytes),
    };
    tokio::spawn(async move {
        let app = Router::new()
            .route("/repos/acme/app/releases/latest", get(serve_gh_latest))
            .route("/repos/acme/app/releases/assets/{id}", get(serve_gh_asset))
            .with_state(api_state);
        let _ = axum::serve(api_listener, app).await;
    });

    let provider = Arc::new(
        GitHubProvider::builder("acme", "app")
            .token("ghp_test_token")
            .api_base_url(api_base)
            .build()
            .expect("github provider"),
    );

    let temp = tempfile::tempdir().expect("temp dir");
    let cache = PackageCache::with_appimage_fallback(temp.path().join("cache"), None);
    let v1_path = temp.path().join("v1.pkg");
    std::fs::write(&v1_path, &v1).expect("write v1");
    cache.seed_from_file(&v1_path, "0.1.0").expect("seed");

    let updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache,
    )
    .target("linux-x86_64")
    .provider(provider)
    .build()
    .expect("updater");

    let update = updater.check().await.expect("gh check").expect("update");
    let pkg = update.download(&DummyListener).await.expect("gh download");
    assert_eq!(pkg.summary.mode, DownloadMode::Differential);
    assert_eq!(std::fs::read(&pkg.path).expect("read"), v2);
    assert_eq!(gh_cdn_check.leaked.load(Ordering::SeqCst), 0);
}
