use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use http::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{Manager, Resource, ResourceId, Runtime, Webview};
use url::Url;

use crate::listener::{DownloadEvent, DownloadListener, DownloadSummary};
use crate::updater::Update;
use crate::{Error, Result, UpdaterPlusExt};

/// Optional parameters passed to the `check` command.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOptions {
    /// Additional HTTP headers sent with manifest requests.
    pub headers: Option<Vec<(String, String)>>,
    /// Request timeout in milliseconds.
    pub timeout: Option<u64>,
    /// Optional HTTP proxy URL.
    pub proxy: Option<String>,
    /// Optional target override (e.g. `linux-x86_64`).
    pub target: Option<String>,
}

/// Optional parameters passed to `download` and `download_and_install`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadOptions {
    /// Additional HTTP headers sent with blockmap and range requests.
    pub headers: Option<Vec<(String, String)>>,
    /// Request timeout in milliseconds.
    pub timeout: Option<u64>,
    /// Whether Windows installers should relaunch the app after completion.
    pub restart_after_install: Option<bool>,
}

/// Optional parameters passed to `install`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOptions {
    /// Whether Windows installers should relaunch the app after completion.
    pub restart_after_install: Option<bool>,
}

/// Metadata returned to the frontend when an update is available.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMetadata {
    rid: ResourceId,
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
    raw_json: serde_json::Value,
}

/// Resource handle and transfer metrics returned by `download`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DownloadResult {
    rid: ResourceId,
    summary: DownloadSummary,
}

struct DownloadedPackageResource {
    path: PathBuf,
    restart_after_install: Option<bool>,
}
impl Resource for DownloadedPackageResource {}

struct ChannelListener {
    channel: Channel<DownloadEvent>,
}

impl DownloadListener for ChannelListener {
    fn on_event(&self, event: DownloadEvent) {
        let _ = self.channel.send(event);
    }
}

#[tauri::command]
pub(crate) async fn check<R: Runtime>(
    webview: Webview<R>,
    options: Option<CheckOptions>,
) -> Result<Option<UpdateMetadata>> {
    let mut builder = webview.updater_plus_builder()?;
    if let Some(opts) = options {
        if let Some(headers) = opts.headers {
            for (key, value) in headers {
                builder = builder.header(&key, &value)?;
            }
        }
        if let Some(timeout_ms) = opts.timeout {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        if let Some(proxy) = opts.proxy {
            builder = builder.proxy(Url::parse(&proxy)?);
        }
        if let Some(target) = opts.target {
            builder = builder.target(target);
        }
    }

    let updater = builder.build()?;
    let Some(update) = updater.check().await? else {
        return Ok(None);
    };

    let metadata = UpdateMetadata {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date: update.date.clone(),
        body: update.body.clone(),
        raw_json: update.raw_json.clone(),
        rid: webview.resources_table().add(update),
    };
    Ok(Some(metadata))
}

impl DownloadOptions {
    fn apply_to(self, mut update: Update) -> Result<(Update, Option<bool>)> {
        if let Some(headers) = self.headers {
            let mut map = update.headers.clone();
            for (key, value) in headers {
                let name = HeaderName::from_str(&key).map_err(|error| {
                    Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
                })?;
                let val = HeaderValue::from_str(&value).map_err(|error| {
                    Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
                })?;
                map.insert(name, val);
            }
            update = update.with_headers(map);
        }
        if let Some(timeout_ms) = self.timeout {
            update = update.with_timeout(Duration::from_millis(timeout_ms));
        }
        if let Some(restart) = self.restart_after_install {
            update = update.restart_after_install(restart);
        }
        Ok((update, self.restart_after_install))
    }
}

#[tauri::command]
pub(crate) async fn download<R: Runtime>(
    webview: Webview<R>,
    rid: ResourceId,
    on_event: Channel<DownloadEvent>,
    options: Option<DownloadOptions>,
) -> Result<DownloadResult> {
    let base = (*webview.resources_table().get::<Update>(rid)?).clone();
    let (update, restart_override) = match options {
        Some(opts) => opts.apply_to(base)?,
        None => (base, None),
    };
    let listener = ChannelListener { channel: on_event };
    let downloaded = update.download(&listener).await?;
    let pkg_rid = webview.resources_table().add(DownloadedPackageResource {
        path: downloaded.path,
        restart_after_install: restart_override,
    });
    Ok(DownloadResult {
        rid: pkg_rid,
        summary: downloaded.summary,
    })
}

#[tauri::command]
pub(crate) async fn install<R: Runtime>(
    webview: Webview<R>,
    update_rid: ResourceId,
    package_rid: ResourceId,
    options: Option<InstallOptions>,
) -> Result<()> {
    let mut update = (*webview.resources_table().get::<Update>(update_rid)?).clone();
    let package = webview
        .resources_table()
        .get::<DownloadedPackageResource>(package_rid)?;
    let restart_choice = options
        .and_then(|o| o.restart_after_install)
        .or(package.restart_after_install);
    if let Some(restart) = restart_choice {
        update = update.restart_after_install(restart);
    }
    let package_path = package.path.clone();
    drop(package);
    tokio::task::spawn_blocking(move || update.install_file(&package_path))
        .await
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;
    let _ = webview.resources_table().close(package_rid);
    Ok(())
}

#[tauri::command]
pub(crate) async fn download_and_install<R: Runtime>(
    webview: Webview<R>,
    rid: ResourceId,
    on_event: Channel<DownloadEvent>,
    options: Option<DownloadOptions>,
) -> Result<DownloadSummary> {
    let base = (*webview.resources_table().get::<Update>(rid)?).clone();
    let (update, _) = match options {
        Some(opts) => opts.apply_to(base)?,
        None => (base, None),
    };
    let listener = ChannelListener { channel: on_event };
    update.download_and_install(&listener).await
}
