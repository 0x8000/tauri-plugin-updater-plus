use axum::Router;
use axum::http::header::CONTENT_RANGE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use url::Url;

use super::super::{HttpDownloader, HttpRequest, RequestStage};
use crate::Error;

#[tokio::test]
async fn enforces_redirect_hop_limit_bounded_body_cap_content_range_validation_and_url_redaction() {
    async fn serve_loop() -> Response {
        Redirect::temporary("/loop?token=secret").into_response()
    }
    async fn serve_oversized() -> Response {
        (StatusCode::OK, vec![b'x'; 2048]).into_response()
    }
    async fn serve_bad_partial() -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, HeaderValue::from_static("bytes 10-19/100"));
        (StatusCode::PARTIAL_CONTENT, headers, vec![0u8; 10]).into_response()
    }
    async fn serve_forbidden() -> Response {
        (StatusCode::FORBIDDEN, "denied").into_response()
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = Url::parse(&format!("http://{}/", listener.local_addr().expect("addr"))).expect("u");
    tokio::spawn(async move {
        let app = Router::new()
            .route("/loop", get(serve_loop))
            .route("/big", get(serve_oversized))
            .route("/bad-partial", get(serve_bad_partial))
            .route("/forbidden", get(serve_forbidden));
        let _ = axum::serve(listener, app).await;
    });

    let downloader = HttpDownloader::builder().build().expect("dl");
    let overrides = crate::provider::TransferOverrides::default();

    let loop_url = base.join("loop?token=super-secret").expect("url");
    let loop_err = downloader
        .send(
            HttpRequest::get(loop_url),
            RequestStage::Artifact,
            &overrides,
        )
        .await
        .expect_err("redirect loop must fail");
    assert!(matches!(
        loop_err,
        Error::DifferentialTransfer(ref msg)
            if msg.contains("exceeded maximum HTTP redirect hops")
                && !msg.contains("super-secret")
                && msg.contains("?<redacted>")
    ));

    let big_url = base.join("big").expect("url");
    let cap_err = downloader
        .fetch_bytes_bounded(
            &big_url,
            RequestStage::Blockmap,
            1024,
            "blockmap",
            &overrides,
        )
        .await
        .expect_err("oversized body must fail");
    assert!(matches!(
        cap_err,
        Error::DifferentialTransfer(ref msg) if msg.contains("exceeds maximum allowed")
    ));

    let forbidden_url = base
        .join("forbidden?X-Amz-Signature=topsecret")
        .expect("url");
    let status_err = downloader
        .fetch_bytes_bounded(
            &forbidden_url,
            RequestStage::Blockmap,
            1024,
            "blockmap",
            &overrides,
        )
        .await
        .expect_err("403 must fail");
    assert!(matches!(
        status_err,
        Error::HttpStatus { status: 403, ref url }
            if !url.contains("topsecret") && url.ends_with("?<redacted>")
    ));

    let bad_partial_url = base.join("bad-partial").expect("url");
    let session = downloader.open_download_session(bad_partial_url, &overrides);
    let partial_err = match session
        .fetch_ranges(vec![crate::planner::ByteRange::new(0, 9).expect("range")])
        .await
    {
        Err(err) => err,
        Ok(_) => panic!("mismatched Content-Range must fail"),
    };
    assert!(matches!(
        partial_err,
        Error::DifferentialTransfer(ref msg) if msg.contains("disagreed with requested")
    ));
}
