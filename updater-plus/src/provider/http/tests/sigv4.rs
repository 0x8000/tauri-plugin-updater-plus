use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::header::{AUTHORIZATION, RANGE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use semver::Version;
use time::OffsetDateTime;
use url::Url;

use super::super::{
    AwsCredentials, HttpRequest, S3Provider, SigV4Authorizer, StaticAwsCredentials,
};
use super::{serve_single_range_or_full, synthetic_v1_v2};
use crate::blockmap::BlockMap;
use crate::cache::PackageCache;
use crate::chunker::ChunkerConfig;
use crate::listener::{DownloadMode, DummyListener};
use crate::updater::UpdaterBuilder;

#[test]
fn sigv4_authorizer_produces_exact_deterministic_aws4_signature_with_and_without_sts() {
    let creds = AwsCredentials {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        session_token: Some("AQoDYXdzEJr///example-sts-token".into()),
    };
    let authorizer =
        SigV4Authorizer::s3("us-east-1", Arc::new(StaticAwsCredentials(creds.clone())));
    let mut req = HttpRequest::get(
        Url::parse(
            "https://releases.s3.us-east-1.amazonaws.com/v0.2.0/MyApp%200.2.0.AppImage?b=2&a=1",
        )
        .expect("url"),
    );
    let fixed_time = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
    authorizer
        .sign_request_at(&mut req, &creds, fixed_time)
        .expect("sign");

    let auth = req
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .expect("auth header");
    assert_eq!(
        auth,
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20231114/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature=8c29574c45cd7d12e62e87c14998a3b218c310353ec2d456ef481a30bb86363d"
    );
    assert_eq!(
        req.headers.get("host").and_then(|v| v.to_str().ok()),
        Some("releases.s3.us-east-1.amazonaws.com")
    );
    assert_eq!(
        req.headers.get("x-amz-date").and_then(|v| v.to_str().ok()),
        Some("20231114T221320Z")
    );
    assert_eq!(
        req.headers
            .get("x-amz-security-token")
            .and_then(|v| v.to_str().ok()),
        Some("AQoDYXdzEJr///example-sts-token")
    );
    assert_eq!(
        req.headers
            .get("x-amz-content-sha256")
            .and_then(|v| v.to_str().ok()),
        Some("UNSIGNED-PAYLOAD")
    );

    // Signing without STS token must remove any stale `x-amz-security-token` header.
    let no_sts = AwsCredentials {
        session_token: None,
        ..creds
    };
    authorizer
        .sign_request_at(&mut req, &no_sts, fixed_time)
        .expect("sign without sts");
    assert!(!req.headers.contains_key("x-amz-security-token"));
    let no_sts_auth = req
        .headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .expect("auth");
    assert!(no_sts_auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
}

#[tokio::test]
async fn s3_provider_signs_every_manifest_blockmap_and_single_range_request() {
    let (v1, v2) = synthetic_v1_v2();
    let v2_art = BlockMap::generate(v2.as_slice(), "file", ChunkerConfig::default()).expect("map");
    let bm_bytes = Arc::new(v2_art.blockmap.to_gzip_bytes().expect("gz"));

    #[derive(Clone)]
    struct S3MockState {
        manifest: Arc<Vec<u8>>,
        blockmap: Arc<Vec<u8>>,
        artifact: Arc<Vec<u8>>,
        signed_hits: Arc<AtomicUsize>,
        multi_range_hits: Arc<AtomicUsize>,
    }

    async fn serve_s3(
        State(state): State<S3MockState>,
        AxumPath(name): AxumPath<String>,
        headers: HeaderMap,
    ) -> Response {
        let auth = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let has_sigv4 = auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIA_TEST/")
            && auth.contains("/us-west-2/s3/aws4_request")
            && auth.contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token",
            )
            && headers
                .get("x-amz-content-sha256")
                .and_then(|v| v.to_str().ok())
                == Some("UNSIGNED-PAYLOAD")
            && headers
                .get("x-amz-security-token")
                .and_then(|v| v.to_str().ok())
                == Some("STS_UNIT_TOKEN")
            && headers.contains_key("x-amz-date");
        if !has_sigv4 {
            return (StatusCode::FORBIDDEN, "missing valid SigV4 headers").into_response();
        }
        state.signed_hits.fetch_add(1, Ordering::SeqCst);
        if let Some(range) = headers.get(RANGE).and_then(|v| v.to_str().ok())
            && range.contains(',')
        {
            state.multi_range_hits.fetch_add(1, Ordering::SeqCst);
        }
        match name.as_str() {
            "latest.json" => (StatusCode::OK, (*state.manifest).clone()).into_response(),
            "app.pkg.blockmap" => (StatusCode::OK, (*state.blockmap).clone()).into_response(),
            "app.pkg" => serve_single_range_or_full(&state.artifact, &headers),
            _ => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base =
        Url::parse(&format!("http://{}/", listener.local_addr().expect("addr"))).expect("url");
    let manifest_json = serde_json::to_vec(&serde_json::json!({
        "version": "0.2.0",
        "url": base.join("app.pkg").expect("url"),
        "sha512": v2_art.sha512,
        "size": v2_art.size,
    }))
    .expect("json");
    let s3_state = S3MockState {
        manifest: Arc::new(manifest_json),
        blockmap: bm_bytes,
        artifact: Arc::new(v2.clone()),
        signed_hits: Arc::new(AtomicUsize::new(0)),
        multi_range_hits: Arc::new(AtomicUsize::new(0)),
    };
    let s3_check = s3_state.clone();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/{name}", get(serve_s3))
            .with_state(s3_state);
        let _ = axum::serve(listener, app).await;
    });

    let creds = Arc::new(StaticAwsCredentials(AwsCredentials {
        access_key_id: "AKIA_TEST".into(),
        secret_access_key: "SECRET_TEST".into(),
        session_token: Some("STS_UNIT_TOKEN".into()),
    }));
    let provider = Arc::new(
        S3Provider::build(
            vec![base.join("latest.json").expect("url")],
            "us-west-2",
            creds,
            None,
        )
        .expect("s3 provider"),
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
    .provider(provider)
    .build()
    .expect("updater");

    let update = updater.check().await.expect("s3 check").expect("update");
    let pkg = update.download(&DummyListener).await.expect("s3 download");
    assert_eq!(pkg.summary.mode, DownloadMode::Differential);
    assert_eq!(std::fs::read(&pkg.path).expect("read"), v2);
    assert_eq!(s3_check.multi_range_hits.load(Ordering::SeqCst), 0);
    assert!(s3_check.signed_hits.load(Ordering::SeqCst) >= 4);
}
