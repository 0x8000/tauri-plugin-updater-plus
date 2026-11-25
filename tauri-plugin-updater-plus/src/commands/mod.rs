use std::path::PathBuf;
#[cfg(feature = "http")]
use std::str::FromStr;
use std::time::Duration;

#[cfg(feature = "http")]
use http::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{Manager, Resource, ResourceId, Runtime, Webview};
use thiserror::Error;
use updater_plus::{DownloadEvent, DownloadListener, DownloadSummary, Error, Update};
use url::Url;

use crate::UpdaterPlusExt;

#[derive(Debug, Error)]
pub(crate) enum CommandError {
    #[error(transparent)]
    Updater(#[from] Error),
    #[error("updater Tauri runtime error: {0}")]
    Tauri(#[from] tauri::Error),
}

impl Serialize for CommandError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

type CommandResult<T> = std::result::Result<T, CommandError>;

/// Optional parameters passed to the `check` command.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOptions {
    /// Additional request metadata/headers sent with manifest requests.
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
    /// Additional request metadata/headers sent with blockmap and range requests.
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

struct UpdateResource(Update);
impl Resource for UpdateResource {}

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
) -> CommandResult<Option<UpdateMetadata>> {
    let mut builder = webview.updater_plus_builder()?;
    if let Some(opts) = options {
        if let Some(headers) = opts.headers {
            for (key, value) in headers {
                #[cfg(feature = "http")]
                {
                    builder = builder.header(&key, &value)?;
                }
                #[cfg(not(feature = "http"))]
                {
                    builder = builder.metadata(key, value);
                }
            }
        }
        if let Some(timeout_ms) = opts.timeout {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        if let Some(proxy) = opts.proxy {
            builder = builder.proxy(Url::parse(&proxy).map_err(Error::from)?);
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
        rid: webview.resources_table().add(UpdateResource(update)),
    };
    Ok(Some(metadata))
}

impl DownloadOptions {
    fn apply_to(self, mut update: Update) -> updater_plus::Result<(Update, Option<bool>)> {
        if let Some(headers) = self.headers {
            for (key, value) in headers {
                #[cfg(feature = "http")]
                {
                    let name = HeaderName::from_str(&key).map_err(|error| {
                        Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
                    })?;
                    let val = HeaderValue::from_str(&value).map_err(|error| {
                        Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
                    })?;
                    let val_str = val.to_str().map_err(|error| {
                        Error::InvalidHeader(format!(
                            "header value for '{key}' is not UTF-8: {error}"
                        ))
                    })?;
                    update = update.with_metadata_entry(name.as_str(), val_str);
                }
                #[cfg(not(feature = "http"))]
                {
                    update = update.with_metadata_entry(key, value);
                }
            }
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
) -> CommandResult<DownloadResult> {
    let base = webview
        .resources_table()
        .get::<UpdateResource>(rid)?
        .0
        .clone();
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
) -> CommandResult<()> {
    let mut update = webview
        .resources_table()
        .get::<UpdateResource>(update_rid)?
        .0
        .clone();
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
) -> CommandResult<DownloadSummary> {
    let base = webview
        .resources_table()
        .get::<UpdateResource>(rid)?
        .0
        .clone();
    let (update, _) = match options {
        Some(opts) => opts.apply_to(base)?,
        None => (base, None),
    };
    let listener = ChannelListener { channel: on_event };
    Ok(update.download_and_install(&listener).await?)
}
