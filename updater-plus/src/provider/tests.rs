use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use futures_util::stream;
use semver::Version;

use super::{
    ArtifactId, BoxFuture, ByteStream, DownloadSession, FailoverProvider, RangeCapability,
    RangeFetchResult, RangePart, ResolveContext, ResolvedRelease, TransferCapabilities,
    TransferOverrides, TransferStats, UpdateProvider,
};
use crate::error::{TransportError, TransportErrorKind};
use crate::planner::ByteRange;
use crate::{Error, Result};

#[derive(Debug)]
struct FailingProvider {
    msg: &'static str,
    calls: Arc<AtomicUsize>,
}

impl UpdateProvider for FailingProvider {
    fn resolve_release<'a>(
        &'a self,
        _ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let msg = self.msg;
        Box::pin(async move {
            Err(Error::Transport(TransportError::new(
                TransportErrorKind::Unavailable,
                msg,
            )))
        })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _max_bytes: usize,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async { unreachable!("failing provider should not serve blockmap") })
    }

    fn open_session<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        Box::pin(async { unreachable!("failing provider should not open session") })
    }
}

#[derive(Debug)]
struct NoUpdateProvider {
    calls: Arc<AtomicUsize>,
}

impl UpdateProvider for NoUpdateProvider {
    fn resolve_release<'a>(
        &'a self,
        _ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _max_bytes: usize,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async { unreachable!() })
    }

    fn open_session<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        Box::pin(async { unreachable!() })
    }
}

#[derive(Debug)]
struct MemoryProvider {
    version: Version,
    sha512: Option<String>,
    artifact: Arc<Vec<u8>>,
    blockmap: Bytes,
}

struct MemorySession {
    data: Arc<Vec<u8>>,
    requests: Arc<AtomicUsize>,
}

impl DownloadSession for MemorySession {
    fn capabilities(&self) -> TransferCapabilities {
        TransferCapabilities {
            range: RangeCapability::MultiRange {
                max_ranges_per_call: 64,
                max_concurrency: 2,
            },
            suggested_max_range_gap: 0,
        }
    }

    fn fetch_full(&self) -> BoxFuture<'static, Result<ByteStream>> {
        let data = Arc::clone(&self.data);
        let requests = Arc::clone(&self.requests);
        Box::pin(async move {
            requests.fetch_add(1, Ordering::SeqCst);
            let len = data.len() as u64;
            let bytes = Bytes::copy_from_slice(&data);
            Ok(ByteStream {
                content_length: Some(len),
                stream: Box::pin(stream::once(async move { Ok(bytes) })),
            })
        })
    }

    fn fetch_ranges(&self, ranges: Vec<ByteRange>) -> BoxFuture<'static, Result<RangeFetchResult>> {
        let data = Arc::clone(&self.data);
        let requests = Arc::clone(&self.requests);
        Box::pin(async move {
            requests.fetch_add(1, Ordering::SeqCst);
            let parts = ranges
                .into_iter()
                .map(|r| RangePart {
                    start: r.start,
                    end: r.end,
                    data: Bytes::copy_from_slice(&data[r.start as usize..=r.end as usize]),
                })
                .collect();
            Ok(RangeFetchResult::Parts(parts))
        })
    }

    fn stats(&self) -> TransferStats {
        TransferStats {
            requests: self.requests.load(Ordering::SeqCst),
        }
    }
}

impl UpdateProvider for MemoryProvider {
    fn resolve_release<'a>(
        &'a self,
        _ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        let version = self.version.clone();
        let sha512 = self.sha512.clone();
        let size = self.artifact.len() as u64;
        Box::pin(async move {
            Ok(Some(ResolvedRelease {
                version,
                notes: Some("in-memory release".into()),
                pub_date: None,
                artifact: ArtifactId::new("mem://app.pkg"),
                blockmap: Some(ArtifactId::new("mem://app.pkg.blockmap")),
                sha512,
                signature: None,
                size: Some(size),
                raw_json: serde_json::json!({"source": "memory"}),
            }))
        })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _max_bytes: usize,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        let bm = self.blockmap.clone();
        Box::pin(async move { Ok(bm) })
    }

    fn open_session<'a>(
        &'a self,
        _id: &'a ArtifactId,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        let data = Arc::clone(&self.artifact);
        Box::pin(async move {
            Ok(Box::new(MemorySession {
                data,
                requests: Arc::new(AtomicUsize::new(0)),
            }) as Box<dyn DownloadSession>)
        })
    }
}

fn default_ctx(require_checksum: bool) -> ResolveContext {
    ResolveContext {
        current_version: Version::new(0, 1, 0),
        target_candidates: vec!["linux-x86_64".into()],
        arch: "x86_64".into(),
        bundle_type: None,
        require_checksum,
        overrides: TransferOverrides::default(),
    }
}

#[test]
fn artifact_id_redacts_query_string_in_display_and_debug() {
    let signed = ArtifactId::new("https://bucket.s3.amazonaws.com/app.pkg?X-Amz-Signature=secret");
    assert_eq!(
        signed.to_string(),
        "https://bucket.s3.amazonaws.com/app.pkg?<redacted>"
    );
    assert!(!format!("{signed:?}").contains("secret"));
    assert_eq!(
        signed.as_str(),
        "https://bucket.s3.amazonaws.com/app.pkg?X-Amz-Signature=secret"
    );
}

#[tokio::test]
async fn failover_provider_handles_nested_routing_validation_short_circuit_and_all_fail() {
    assert!(matches!(
        FailoverProvider::new(vec![]),
        Err(Error::InvalidConfig(_))
    ));

    let primary_calls = Arc::new(AtomicUsize::new(0));
    let failing = Arc::new(FailingProvider {
        msg: "primary broker unreachable",
        calls: Arc::clone(&primary_calls),
    });
    let whitespace_unverified = Arc::new(MemoryProvider {
        version: Version::new(0, 2, 0),
        sha512: Some("   ".into()),
        artifact: Arc::new(b"unverified".to_vec()),
        blockmap: Bytes::from_static(b"bad"),
    });
    let healthy = Arc::new(MemoryProvider {
        version: Version::new(0, 2, 0),
        sha512: Some("valid-sha512".into()),
        artifact: Arc::new(b"healthy-payload".to_vec()),
        blockmap: Bytes::from_static(b"good-blockmap"),
    });

    // Nested FailoverProvider exercises LIFO route stack propagation across layers.
    let inner = Arc::new(
        FailoverProvider::new(vec![whitespace_unverified, healthy]).expect("inner failover"),
    );
    let outer = FailoverProvider::new(vec![failing, inner]).expect("outer failover");

    let release = outer
        .resolve_release(&default_ctx(true))
        .await
        .expect("failover resolve")
        .expect("release");
    assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(release.sha512.as_deref(), Some("valid-sha512"));
    assert_eq!(release.artifact.as_str(), "mem://app.pkg");

    let bm = outer
        .fetch_blockmap(
            release.blockmap.as_ref().expect("blockmap"),
            1024,
            &TransferOverrides::default(),
        )
        .await
        .expect("routed blockmap");
    assert_eq!(bm, Bytes::from_static(b"good-blockmap"));

    let session = outer
        .open_session(&release.artifact, &TransferOverrides::default())
        .await
        .expect("routed session");
    let res = session
        .fetch_ranges(vec![ByteRange::new(0, 6).expect("range")])
        .await
        .expect("range");
    match res {
        RangeFetchResult::Parts(parts) => assert_eq!(parts[0].data.as_ref(), b"healthy"),
        _ => panic!("expected Parts"),
    }
    assert_eq!(session.stats().requests, 1);

    // Ok(None) (e.g. 204 No Content) short-circuits without querying subsequent mirrors.
    let c1 = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::new(AtomicUsize::new(0));
    let short_circuit = FailoverProvider::new(vec![
        Arc::new(NoUpdateProvider {
            calls: Arc::clone(&c1),
        }),
        Arc::new(FailingProvider {
            msg: "must not be called",
            calls: Arc::clone(&c2),
        }),
    ])
    .expect("short-circuit");
    assert!(
        short_circuit
            .resolve_release(&default_ctx(true))
            .await
            .expect("ok none")
            .is_none()
    );
    assert_eq!(c1.load(Ordering::SeqCst), 1);
    assert_eq!(c2.load(Ordering::SeqCst), 0);

    // When all children fail, returns the last child's error.
    let all_fail = FailoverProvider::new(vec![
        Arc::new(FailingProvider {
            msg: "first error",
            calls: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(FailingProvider {
            msg: "last error",
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    ])
    .expect("all fail");
    let err = all_fail
        .resolve_release(&default_ctx(true))
        .await
        .expect_err("all fail must error");
    assert!(
        matches!(
            err,
            Error::Transport(ref t)
                if t.kind == TransportErrorKind::Unavailable && t.message == "last error"
        ),
        "expected Unavailable TransportError('last error'), got {err:?}"
    );
}
