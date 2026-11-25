use std::sync::Arc;

use http::StatusCode;
use http::header::{HeaderName, HeaderValue};
use updater_plus::{
    AwsCredentials, BoxFuture, Error, FailoverProvider, GitHubProvider, HttpDownloader,
    HttpMiddleware, HttpNext, HttpRequest, HttpResponse, RequestStage, Result, S3Provider,
    StaticAwsCredentials, StaticHttpProvider, TransferOptions, UpdateProvider,
};

use crate::RunSpec;
use crate::mqtt::MqttPullProvider;

#[derive(Debug)]
struct Retry503Middleware {
    max_retries: usize,
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

pub(crate) fn build_custom_provider(spec: &RunSpec) -> Result<Option<Arc<dyn UpdateProvider>>> {
    let transfer_opts = TransferOptions {
        max_ranges_per_request: spec.config.max_ranges_per_request,
        use_multiple_range_request: spec.config.use_multiple_range_request,
        max_concurrent_ranges: spec.config.max_concurrent_ranges,
    };

    match spec.provider.kind.as_str() {
        "static_http" => Ok(None),
        "mqtt_pull" => {
            let broker = spec.provider.broker_addr.clone().ok_or_else(|| {
                Error::InvalidConfig("mqtt_pull requires provider.brokerAddr".into())
            })?;
            Ok(Some(Arc::new(MqttPullProvider::new(broker))))
        }
        "failover_mqtt_to_http" => {
            let broker = spec.provider.broker_addr.clone().ok_or_else(|| {
                Error::InvalidConfig("failover_mqtt_to_http requires provider.brokerAddr".into())
            })?;
            let mqtt: Arc<dyn UpdateProvider> = Arc::new(MqttPullProvider::new(broker));
            let downloader = HttpDownloader::builder()
                .transfer_options(transfer_opts)
                .build()?;
            let http: Arc<dyn UpdateProvider> = Arc::new(StaticHttpProvider::new(
                downloader,
                spec.config.endpoints.clone(),
            )?);
            Ok(Some(Arc::new(FailoverProvider::new(vec![mqtt, http])?)))
        }
        "github" => {
            let owner = spec.provider.owner.clone().unwrap_or_else(|| "acme".into());
            let repo = spec.provider.repo.clone().unwrap_or_else(|| "myapp".into());
            let mut builder = GitHubProvider::builder(owner, repo);
            if let Some(name) = &spec.provider.manifest_name {
                builder = builder.manifest_name(name.clone());
            }
            if let Some(token) = &spec.provider.token {
                builder = builder.token(token.clone());
            }
            if let Some(api_base) = &spec.provider.api_base_url {
                builder = builder.api_base_url(api_base.clone());
            }
            Ok(Some(Arc::new(builder.build()?)))
        }
        "s3" => {
            let region = spec
                .provider
                .region
                .clone()
                .unwrap_or_else(|| "us-east-1".into());
            let creds = Arc::new(StaticAwsCredentials(AwsCredentials {
                access_key_id: spec
                    .provider
                    .access_key_id
                    .clone()
                    .unwrap_or_else(|| "AKIA_BDD_KEY".into()),
                secret_access_key: spec
                    .provider
                    .secret_access_key
                    .clone()
                    .unwrap_or_else(|| "BDD_SECRET_ACCESS_KEY".into()),
                session_token: spec.provider.session_token.clone(),
            }));
            let downloader = HttpDownloader::builder()
                .transfer_options(transfer_opts)
                .build()?;
            let provider = S3Provider::build(
                spec.config.endpoints.clone(),
                region,
                creds,
                Some(downloader),
            )?;
            Ok(Some(Arc::new(provider)))
        }
        "http_middleware_retry" => {
            let api_key = spec
                .provider
                .api_key
                .clone()
                .unwrap_or_else(|| "bdd-api-key-secret".into());
            let max_retries = spec.provider.max_retries.unwrap_or(3);
            let downloader = HttpDownloader::builder()
                .transfer_options(transfer_opts)
                .sensitive_header(HeaderName::from_static("x-api-key"))
                .with_authorizer(move |ctx, req| {
                    if !ctx.is_cross_origin_redirect {
                        let val = HeaderValue::from_str(&api_key).map_err(|err| {
                            Error::InvalidHeader(format!("invalid x-api-key: {err}"))
                        })?;
                        req.headers
                            .insert(HeaderName::from_static("x-api-key"), val);
                    }
                    Ok(())
                })
                .middleware(Arc::new(Retry503Middleware { max_retries }))
                .build()?;
            let provider = StaticHttpProvider::new(downloader, spec.config.endpoints.clone())?;
            Ok(Some(Arc::new(provider)))
        }
        other => Err(Error::InvalidConfig(format!(
            "unsupported provider kind '{other}'"
        ))),
    }
}
