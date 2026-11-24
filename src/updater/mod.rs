use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};
use semver::Version;
use url::Url;

use crate::cache::PackageCache;
use crate::config::Config;
use crate::manifest::ReleaseManifest;
use crate::{Error, Result};

mod target;
#[cfg(test)]
mod tests;
mod update;

pub use target::*;
pub use update::*;

/// Builder for configuring an [`Updater`] instance before checking endpoints.
#[derive(Debug, Clone)]
pub struct UpdaterBuilder {
    app_name: String,
    current_version: Version,
    extract_path: PathBuf,
    cache: PackageCache,
    config: Config,
    target: Option<String>,
    headers: HeaderMap,
    timeout: Option<Duration>,
    proxy: Option<Url>,
}

impl UpdaterBuilder {
    /// Creates an updater builder from core application coordinates and plugin configuration.
    pub fn new(
        app_name: String,
        current_version: Version,
        extract_path: PathBuf,
        cache: PackageCache,
    ) -> Self {
        Self {
            app_name,
            current_version,
            extract_path,
            cache,
            config: Config::default(),
            target: None,
            headers: HeaderMap::new(),
            timeout: None,
            proxy: None,
        }
    }

    /// Replaces the updater configuration.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Overrides the endpoint list from configuration.
    pub fn endpoints(mut self, endpoints: Vec<Url>) -> Self {
        self.config.endpoints = endpoints;
        self
    }

    /// Overrides the target triple used for manifest platform selection.
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Appends an HTTP request header sent to manifest and artifact endpoints.
    pub fn header(mut self, key: &str, value: &str) -> Result<Self> {
        let name = HeaderName::from_str(key).map_err(|error| {
            Error::InvalidHeader(format!("invalid header name '{key}': {error}"))
        })?;
        let val = HeaderValue::from_str(value).map_err(|error| {
            Error::InvalidHeader(format!("invalid header value for '{key}': {error}"))
        })?;
        self.headers.append(name, val);
        Ok(self)
    }

    /// Sets the HTTP request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Routes HTTP requests through an explicit proxy URL.
    pub fn proxy(mut self, proxy: Url) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Builds a validated [`Updater`].
    pub fn build(self) -> Result<Updater> {
        if self.config.endpoints.is_empty() {
            return Err(Error::InvalidConfig(
                "at least one updater endpoint must be configured".into(),
            ));
        }
        Ok(Updater {
            app_name: self.app_name,
            current_version: self.current_version,
            extract_path: self.extract_path,
            cache: self.cache,
            config: self.config,
            target: self.target,
            headers: self.headers,
            timeout: self.timeout,
            proxy: self.proxy,
        })
    }
}

/// Checks remote manifest endpoints and constructs downloadable [`Update`] handles.
#[derive(Debug, Clone)]
pub struct Updater {
    app_name: String,
    current_version: Version,
    extract_path: PathBuf,
    cache: PackageCache,
    config: Config,
    target: Option<String>,
    headers: HeaderMap,
    timeout: Option<Duration>,
    proxy: Option<Url>,
}

impl Updater {
    /// Checks configured endpoints in order and returns the first newer release found.
    pub async fn check(&self) -> Result<Option<Update>> {
        let client = self.build_client()?;
        let candidates = TargetResolver::target_candidates(self.target.as_deref());
        let primary_target = candidates
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let current_ver_str = self.current_version.to_string();

        let mut last_error: Option<Error> = None;
        for endpoint in &self.config.endpoints {
            let expanded =
                TargetResolver::expand_endpoint(endpoint, &primary_target, &current_ver_str)?;
            match self.fetch_manifest(&client, &expanded).await {
                Ok(None) => return Ok(None),
                Ok(Some(manifest)) => {
                    if !self.should_install(&manifest.version) {
                        return Ok(None);
                    }
                    let platform = manifest.resolve_platform(&candidates)?;
                    let blockmap_url = platform.resolved_blockmap_url()?;
                    return Ok(Some(Update {
                        current_version: current_ver_str,
                        version: manifest.version.to_string(),
                        date: manifest.formatted_pub_date(),
                        body: manifest.notes.clone(),
                        raw_json: manifest.raw_json.clone(),
                        download_url: platform.url.clone(),
                        blockmap_url,
                        sha512: platform.sha512.clone(),
                        signature: platform.signature.clone(),
                        size: platform.size,
                        app_name: self.app_name.clone(),
                        extract_path: self.extract_path.clone(),
                        cache: self.cache.clone(),
                        headers: self.headers.clone(),
                        timeout: self.timeout,
                        proxy: self.proxy.clone(),
                        config: self.config.clone(),
                        restart_after_install: true,
                    }));
                }
                Err(error) => {
                    tracing::warn!(endpoint = %expanded, error = %error, "updater endpoint check failed");
                    last_error = Some(error);
                }
            }
        }

        match last_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    fn should_install(&self, remote: &Version) -> bool {
        if self.config.allow_downgrades {
            remote != &self.current_version
        } else {
            remote > &self.current_version
        }
    }

    async fn fetch_manifest(
        &self,
        client: &reqwest::Client,
        url: &Url,
    ) -> Result<Option<ReleaseManifest>> {
        let response = client
            .get(url.clone())
            .headers(self.headers.clone())
            .send()
            .await?;
        if response.status() == http::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error::HttpStatus {
                url: url.to_string(),
                status: response.status().as_u16(),
            });
        }
        let body = response.text().await?;
        Ok(Some(ReleaseManifest::parse(&body, url)?))
    }

    fn build_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder().user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ));
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        if let Some(proxy_url) = &self.proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy_url.as_str())?);
        }
        Ok(builder.build()?)
    }
}
