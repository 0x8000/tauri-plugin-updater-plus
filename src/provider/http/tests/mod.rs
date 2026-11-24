use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{AUTHORIZATION, CONTENT_RANGE, HeaderName, RANGE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use semver::Version;
use url::Url;

use super::{
    HttpDownloader, HttpMiddleware, HttpNext, HttpRequest, HttpResponse, RequestStage,
    StaticHttpProvider,
};
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadMode, DummyListener};
use crate::provider::BoxFuture;
use crate::updater::UpdaterBuilder;
use crate::{Error, Result};

mod github;
mod sigv4;

pub(super) fn synthetic_v1_v2() -> (Vec<u8>, Vec<u8>) {
    let mut state = 0x7788_99aau64;
    let mut v1 = Vec::with_capacity(900_000);
    for _ in 0..900_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        v1.push((state >> 56) as u8);
    }
    let mut v2 = v1.clone();
    v2[120_000..122_048].fill(0x33);
    v2[620_000..622_048].fill(0x77);
    (v1, v2)
}

pub(super) fn serve_single_range_or_full(data: &[u8], headers: &HeaderMap) -> Response {
    let Some(range_hdr) = headers.get(RANGE).and_then(|v| v.to_str().ok()) else {
        return (StatusCode::OK, data.to_vec()).into_response();
    };
    let spec = range_hdr.trim_start_matches("bytes=");
    if spec.contains(',') {
        return (StatusCode::OK, data.to_vec()).into_response();
    }
    let Some((s, e)) = spec.split_once('-') else {
        return (StatusCode::BAD_REQUEST, "bad range").into_response();
    };
    let (Ok(start), Ok(end)) = (s.parse::<usize>(), e.parse::<usize>()) else {
        return (StatusCode::BAD_REQUEST, "bad range").into_response();
    };
    let mut resp_headers = HeaderMap::new();
    resp_headers.insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{}", data.len())).expect("hdr"),
    );
    (
        StatusCode::PARTIAL_CONTENT,
        resp_headers,
        data[start..=end].to_vec(),
    )
        .into_response()
}

#[derive(Debug)]
struct Retry503Middleware {
    max_retries: usize,
    attempts: Arc<AtomicUsize>,
}

impl HttpMiddleware for Retry503Middleware {
    fn handle<'a>(
        &'a self,
        req: HttpRequest,
        stage: RequestStage,
        next: HttpNext<'a>,
    ) -> BoxFuture<'a, Result<HttpResponse>> {
        Box::pin(async move {
            let mut retries = 0usize;
            loop {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                let resp = next.run(req.clone(), stage).await?;
                if resp.status() == StatusCode::SERVICE_UNAVAILABLE && retries < self.max_retries {
                    retries += 1;
                    continue;
                }
                return Ok(resp);
            }
        })
    }
}

#[tokio::test]
async fn middleware_retries_transient_503_and_strips_both_static_and_dynamic_sensitive_headers() {
    let (v1, v2) = synthetic_v1_v2();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let bm_bytes = Arc::new(v2_art.blockmap.to_gzip_bytes().expect("gz"));

    #[derive(Clone)]
    struct CdnState {
        artifact: Arc<Vec<u8>>,
        leaked_custom_header: Arc<AtomicUsize>,
    }

    async fn serve_cdn(State(state): State<CdnState>, headers: HeaderMap) -> Response {
        if headers.contains_key("x-api-key")
            || headers.contains_key("x-static-secret")
            || headers.contains_key(AUTHORIZATION)
        {
            state.leaked_custom_header.fetch_add(1, Ordering::SeqCst);
            return (StatusCode::BAD_REQUEST, "custom credential leaked to CDN").into_response();
        }
        serve_single_range_or_full(&state.artifact, &headers)
    }

    let cdn_state = CdnState {
        artifact: Arc::new(v2.clone()),
        leaked_custom_header: Arc::new(AtomicUsize::new(0)),
    };
    let cdn_state_check = cdn_state.clone();
    let cdn_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cdn");
    let cdn_url = Url::parse(&format!(
        "http://{}/cdn/app.pkg",
        cdn_listener.local_addr().expect("addr")
    ))
    .expect("cdn url");
    tokio::spawn(async move {
        let app = Router::new()
            .route("/cdn/app.pkg", get(serve_cdn))
            .with_state(cdn_state);
        let _ = axum::serve(cdn_listener, app).await;
    });

    #[derive(Clone)]
    struct OriginState {
        manifest: Arc<Vec<u8>>,
        blockmap: Arc<Vec<u8>>,
        cdn_url: Url,
        manifest_hits: Arc<AtomicUsize>,
        origin_auth_hits: Arc<AtomicUsize>,
    }

    async fn serve_origin(
        State(state): State<OriginState>,
        AxumPath(name): AxumPath<String>,
        headers: HeaderMap,
    ) -> Response {
        if headers.get("x-api-key").and_then(|v| v.to_str().ok()) != Some("top-secret-key")
            || headers.get("x-static-secret").and_then(|v| v.to_str().ok())
                != Some("static-origin-secret")
        {
            return (StatusCode::UNAUTHORIZED, "missing origin credentials").into_response();
        }
        state.origin_auth_hits.fetch_add(1, Ordering::SeqCst);
        if name == "always-503.json" {
            return (StatusCode::SERVICE_UNAVAILABLE, "still overloaded").into_response();
        }
        if name == "latest.json" {
            let hit = state.manifest_hits.fetch_add(1, Ordering::SeqCst);
            if hit == 0 {
                return (StatusCode::SERVICE_UNAVAILABLE, "transient overload").into_response();
            }
            return (StatusCode::OK, (*state.manifest).clone()).into_response();
        }
        if name == "app.pkg.blockmap" {
            return (StatusCode::OK, (*state.blockmap).clone()).into_response();
        }
        if name == "app.pkg" {
            return Redirect::temporary(state.cdn_url.as_str()).into_response();
        }
        (StatusCode::NOT_FOUND, "not found").into_response()
    }

    let origin_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind origin");
    let origin_base = Url::parse(&format!(
        "http://{}/",
        origin_listener.local_addr().expect("addr")
    ))
    .expect("origin url");
    let manifest_json = serde_json::json!({
        "version": "0.2.0",
        "url": origin_base.join("app.pkg").expect("url"),
        "sha512": v2_art.sha512,
        "size": v2_art.size,
    });
    let origin_state = OriginState {
        manifest: Arc::new(serde_json::to_vec(&manifest_json).expect("json")),
        blockmap: bm_bytes,
        cdn_url,
        manifest_hits: Arc::new(AtomicUsize::new(0)),
        origin_auth_hits: Arc::new(AtomicUsize::new(0)),
    };
    let origin_state_ref = origin_state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/{name}", get(serve_origin))
            .with_state(origin_state);
        let _ = axum::serve(origin_listener, app).await;
    });

    let retry_attempts = Arc::new(AtomicUsize::new(0));
    let downloader = HttpDownloader::builder()
        .header("x-static-secret", "static-origin-secret")
        .expect("header")
        .sensitive_header(HeaderName::from_static("x-static-secret"))
        .sensitive_header(HeaderName::from_static("x-api-key"))
        .with_authorizer(|ctx, req| {
            if !ctx.is_cross_origin_redirect {
                req.headers.insert(
                    HeaderName::from_static("x-api-key"),
                    HeaderValue::from_static("top-secret-key"),
                );
            }
            Ok(())
        })
        .middleware(Arc::new(Retry503Middleware {
            max_retries: 2,
            attempts: Arc::clone(&retry_attempts),
        }))
        .build()
        .expect("downloader");

    let provider = Arc::new(
        StaticHttpProvider::new(
            downloader.clone(),
            vec![origin_base.join("latest.json").expect("url")],
        )
        .expect("provider"),
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
        cache.clone(),
    )
    .provider(provider)
    .build()
    .expect("updater");

    let update = updater.check().await.expect("check").expect("update");
    assert_eq!(origin_state_ref.manifest_hits.load(Ordering::SeqCst), 2);
    let pkg = update.download(&DummyListener).await.expect("download");
    assert_eq!(pkg.summary.mode, DownloadMode::Differential);
    assert_eq!(std::fs::read(&pkg.path).expect("read"), v2);
    assert_eq!(
        cdn_state_check.leaked_custom_header.load(Ordering::SeqCst),
        0
    );
    assert!(origin_state_ref.origin_auth_hits.load(Ordering::SeqCst) >= 3);
    assert!(retry_attempts.load(Ordering::SeqCst) >= 4);

    let before_exhaust = retry_attempts.load(Ordering::SeqCst);
    let failing_provider = Arc::new(
        StaticHttpProvider::new(
            downloader,
            vec![origin_base.join("always-503.json").expect("url")],
        )
        .expect("failing provider"),
    );
    let failing_updater = UpdaterBuilder::new(
        "myapp".into(),
        Version::new(0, 1, 0),
        temp.path().join("app"),
        cache,
    )
    .provider(failing_provider)
    .build()
    .expect("updater");
    assert!(matches!(
        failing_updater.check().await,
        Err(Error::HttpStatus { status: 503, .. })
    ));
    assert_eq!(retry_attempts.load(Ordering::SeqCst) - before_exhaust, 3);
}
