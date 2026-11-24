use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use futures_util::stream;
use semver::Version;
use tauri_plugin_updater_plus::{
    ArtifactId, BlockMap, BoxFuture, ByteRange, ByteStream, DiffSession, DownloadEvent,
    DownloadListener, DownloadSession, Error, InstallContext, PackageCache, PlatformInstaller,
    RangeCapability, RangeFetchResult, RangePart, ResolveContext, ResolvedRelease, Result,
    TransferCapabilities, TransferOverrides, TransferStats, TransportError, TransportErrorKind,
    UpdateProvider, VerifyPolicy,
};
use tokio::sync::Mutex as AsyncMutex;

use crate::mqtt_wire::MqttWireClient;
use crate::{RecordingListener, RunReport, RunSpec};

#[derive(Debug)]
pub(crate) struct MqttPullProvider {
    broker_addr: String,
    client_id: String,
    req_seq: Arc<AtomicU64>,
}

impl MqttPullProvider {
    pub(crate) fn new(broker_addr: String) -> Self {
        Self {
            broker_addr,
            client_id: "updater-pull".into(),
            req_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    async fn rpc(&self, mut body: serde_json::Value) -> Result<Vec<u8>> {
        let req_id = self.req_seq.fetch_add(1, Ordering::SeqCst).to_string();
        body["req_id"] = serde_json::Value::String(req_id.clone());
        let mut client = MqttWireClient::connect(&self.broker_addr, &self.client_id).await?;
        client
            .subscribe(&format!("updates/resp/{}/{req_id}", self.client_id))
            .await?;
        client
            .publish(
                &format!("updates/req/{}", self.client_id),
                &serde_json::to_vec(&body)?,
            )
            .await?;
        let (_, resp) = client.next_publish().await?;
        Ok(resp)
    }
}

#[derive(Clone)]
struct MqttSessionHandle {
    broker_addr: String,
    client_id: String,
    artifact_id: String,
    conn: Arc<AsyncMutex<Option<MqttWireClient>>>,
    req_seq: Arc<AtomicU64>,
    requests: Arc<AtomicUsize>,
}

impl MqttSessionHandle {
    async fn rpc(&self, mut payload: serde_json::Value) -> Result<Vec<u8>> {
        let req_id = self.req_seq.fetch_add(1, Ordering::SeqCst).to_string();
        payload["req_id"] = serde_json::Value::String(req_id);

        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            let mut client = MqttWireClient::connect(&self.broker_addr, &self.client_id).await?;
            client
                .subscribe(&format!("updates/resp/{}/#", self.client_id))
                .await?;
            *guard = Some(client);
        }
        let client = guard.as_mut().expect("connected mqtt client");
        client
            .publish(
                &format!("updates/req/{}", self.client_id),
                &serde_json::to_vec(&payload)?,
            )
            .await?;
        let (_, resp) = client.next_publish().await?;
        Ok(resp)
    }
}

struct MqttDownloadSession {
    handle: MqttSessionHandle,
}

impl DownloadSession for MqttDownloadSession {
    fn capabilities(&self) -> TransferCapabilities {
        TransferCapabilities {
            range: RangeCapability::MultiRange {
                max_ranges_per_call: 64,
                max_concurrency: 1,
            },
            suggested_max_range_gap: 0,
        }
    }

    fn fetch_full(&self) -> BoxFuture<'static, Result<ByteStream>> {
        let handle = self.handle.clone();
        Box::pin(async move {
            handle.requests.fetch_add(1, Ordering::SeqCst);
            let resp = handle
                .rpc(serde_json::json!({ "op": "full", "id": handle.artifact_id }))
                .await?;
            if resp.first() != Some(&0) {
                return Err(Error::Transport(TransportError::new(
                    TransportErrorKind::NotFound,
                    String::from_utf8_lossy(resp.get(1..).unwrap_or_default()).into_owned(),
                )));
            }
            let payload = Bytes::copy_from_slice(&resp[1..]);
            let len = payload.len() as u64;
            Ok(ByteStream {
                content_length: Some(len),
                stream: Box::pin(stream::once(async move { Ok(payload) })),
            })
        })
    }

    fn fetch_ranges(&self, ranges: Vec<ByteRange>) -> BoxFuture<'static, Result<RangeFetchResult>> {
        let handle = self.handle.clone();
        Box::pin(async move {
            handle.requests.fetch_add(1, Ordering::SeqCst);
            let specs: Vec<[u64; 2]> = ranges.iter().map(|r| [r.start, r.end]).collect();
            let resp = handle
                .rpc(serde_json::json!({
                    "op": "ranges",
                    "id": handle.artifact_id,
                    "ranges": specs,
                }))
                .await?;
            if resp.first() != Some(&0) || resp.len() < 5 {
                return Err(Error::Transport(TransportError::new(
                    TransportErrorKind::Unavailable,
                    String::from_utf8_lossy(resp.get(1..).unwrap_or_default()).into_owned(),
                )));
            }
            let count = u32::from_be_bytes([resp[1], resp[2], resp[3], resp[4]]) as usize;
            let mut pos = 5usize;
            let mut parts = Vec::with_capacity(count);
            for _ in 0..count {
                let start = u64::from_be_bytes(resp[pos..pos + 8].try_into().expect("start"));
                let end = u64::from_be_bytes(resp[pos + 8..pos + 16].try_into().expect("end"));
                let len =
                    u32::from_be_bytes(resp[pos + 16..pos + 20].try_into().expect("len")) as usize;
                pos += 20;
                let data = Bytes::copy_from_slice(&resp[pos..pos + len]);
                pos += len;
                parts.push(RangePart { start, end, data });
            }
            Ok(RangeFetchResult::Parts(parts))
        })
    }

    fn stats(&self) -> TransferStats {
        TransferStats {
            requests: self.handle.requests.load(Ordering::SeqCst),
        }
    }
}

impl UpdateProvider for MqttPullProvider {
    fn resolve_release<'a>(
        &'a self,
        ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        Box::pin(async move {
            if ctx.overrides.proxy.is_some() {
                return Err(Error::InvalidConfig(
                    "HTTP proxy override is not supported by MqttPullProvider".into(),
                ));
            }
            let raw = self.rpc(serde_json::json!({ "op": "manifest" })).await?;
            let json: serde_json::Value = serde_json::from_slice(&raw)?;
            if !json.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                let msg = json
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("MQTT manifest fetch failed");
                return Err(Error::Transport(TransportError::new(
                    TransportErrorKind::Unavailable,
                    msg,
                )));
            }
            let rel = &json["release"];
            let version = Version::parse(rel["version"].as_str().unwrap_or("0.0.0"))?;
            Ok(Some(ResolvedRelease {
                version,
                notes: rel
                    .get("notes")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                pub_date: None,
                artifact: ArtifactId::new(rel["artifact"].as_str().unwrap_or("")),
                blockmap: rel
                    .get("blockmap")
                    .and_then(|v| v.as_str())
                    .map(ArtifactId::new),
                sha512: rel
                    .get("sha512")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                signature: rel
                    .get("signature")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                size: rel.get("size").and_then(|v| v.as_u64()),
                raw_json: rel.clone(),
            }))
        })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        id: &'a ArtifactId,
        max_bytes: usize,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            let raw = self
                .rpc(serde_json::json!({ "op": "blockmap", "id": id.as_str() }))
                .await?;
            if raw.first() != Some(&0) {
                let kind = if raw.first() == Some(&2) {
                    TransportErrorKind::NotFound
                } else {
                    TransportErrorKind::Unavailable
                };
                return Err(Error::Transport(TransportError::new(
                    kind,
                    String::from_utf8_lossy(raw.get(1..).unwrap_or_default()).into_owned(),
                )));
            }
            if raw.len() - 1 > max_bytes {
                return Err(Error::DifferentialTransfer(format!(
                    "MQTT blockmap exceeded {max_bytes} bytes"
                )));
            }
            Ok(Bytes::copy_from_slice(&raw[1..]))
        })
    }

    fn open_session<'a>(
        &'a self,
        id: &'a ArtifactId,
        _overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        let handle = MqttSessionHandle {
            broker_addr: self.broker_addr.clone(),
            client_id: format!("{}-sess", self.client_id),
            artifact_id: id.as_str().to_string(),
            conn: Arc::new(AsyncMutex::new(None)),
            req_seq: Arc::clone(&self.req_seq),
            requests: Arc::new(AtomicUsize::new(0)),
        };
        Box::pin(
            async move { Ok(Box::new(MqttDownloadSession { handle }) as Box<dyn DownloadSession>) },
        )
    }
}

pub(crate) async fn execute_mqtt_push(spec: RunSpec, cache: PackageCache) -> Result<RunReport> {
    let broker_addr = spec
        .provider
        .broker_addr
        .ok_or_else(|| Error::InvalidConfig("mqtt_push requires provider.brokerAddr".into()))?;
    let client_id = "updater-push";
    let mut client = MqttWireClient::connect(&broker_addr, client_id).await?;
    client
        .subscribe(&format!("updates/push/{client_id}/#"))
        .await?;
    client
        .publish(
            &format!("updates/push/hello/{client_id}"),
            br#"{"action":"subscribe_release"}"#,
        )
        .await?;

    let (topic, announce_raw) = client.next_publish().await?;
    if !topic.ends_with("/announce") {
        return Err(Error::Transport(TransportError::new(
            TransportErrorKind::Other,
            format!("expected announce topic, got {topic}"),
        )));
    }
    let announce: serde_json::Value = serde_json::from_slice(&announce_raw)?;
    let version = announce["version"].as_str().unwrap_or("0.0.0").to_string();
    let sha512 = announce["sha512"].as_str().unwrap_or("").to_string();
    let bm_bytes = STANDARD
        .decode(announce["blockmap_b64"].as_str().unwrap_or(""))
        .map_err(|e| Error::InvalidBlockMap(e.to_string()))?;
    let remote_blockmap = BlockMap::from_bytes(&bm_bytes)?;

    let policy = VerifyPolicy::Sha512 { expected: sha512 };
    let max_gap = spec.config.max_range_gap.unwrap_or(0);
    let (mut session, plan) = DiffSession::open(
        cache.clone(),
        version.clone(),
        remote_blockmap,
        policy,
        max_gap,
    )?;

    let listener = RecordingListener::default();
    let want_ranges: Vec<[u64; 2]> = plan
        .missing_ranges
        .iter()
        .map(|r| [r.start, r.end])
        .collect();
    client
        .publish(
            &format!("updates/push/want/{client_id}"),
            &serde_json::to_vec(&serde_json::json!({ "ranges": want_ranges }))?,
        )
        .await?;

    let mut chunk_messages = 0usize;
    loop {
        let (msg_topic, payload) = client.next_publish().await?;
        if msg_topic.ends_with("/done") {
            break;
        }
        if msg_topic.ends_with("/chunk") {
            chunk_messages += 1;
            let offset = u64::from_be_bytes(payload[..8].try_into().expect("offset"));
            let newly_covered = session.write_range(offset, &payload[8..])?;
            if newly_covered > 0 {
                listener.on_event(DownloadEvent::Progress {
                    chunk_length: newly_covered,
                });
            }
        }
    }

    let downloaded = session.finish_with_stats(chunk_messages)?;
    if spec.action == "download_and_install" || spec.action == "download_then_install" {
        PlatformInstaller::install(
            &downloaded.path,
            &InstallContext {
                app_name: spec.app_name,
                version: version.clone(),
                extract_path: spec.extract_path,
                restart_after_install: true,
                windows: spec.config.windows,
            },
        )?;
    }

    let events = listener.events.lock().expect("Mutex poisoned");
    let progress_bytes_total = events
        .iter()
        .filter_map(|event| match event {
            DownloadEvent::Progress { chunk_length } => Some(*chunk_length),
            _ => None,
        })
        .fold(0usize, usize::saturating_add);
    let state = cache.state();

    Ok(RunReport {
        status: "ok",
        update_available: true,
        version: Some(version),
        notes: None,
        summary: Some(downloaded.summary),
        events_count: events.len(),
        started_differential: Some(true),
        progress_bytes_total,
        has_finished_event: false,
        cached_version: state.as_ref().map(|s| s.version.clone()),
        cached_size: state.as_ref().map(|s| s.size),
    })
}
