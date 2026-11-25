use std::sync::Arc;

use bytes::Bytes;

use super::{
    ArtifactId, BoxFuture, DownloadSession, ResolveContext, ResolvedRelease, TransferOverrides,
    UpdateProvider,
};
use crate::{Error, Result};

/// Composite [`UpdateProvider`] that tries multiple child providers in order on failure.
///
/// Works seamlessly across homogeneous mirrors (e.g. multiple HTTP CDNs) as well as heterogeneous
/// transports (e.g. a local MQTT/gRPC mirror failing over to a public Nginx/S3 mirror).
#[derive(Debug, Clone)]
pub struct FailoverProvider {
    providers: Vec<Arc<dyn UpdateProvider>>,
}

impl FailoverProvider {
    /// Creates a failover provider from an ordered list of candidate providers.
    pub fn new(providers: Vec<Arc<dyn UpdateProvider>>) -> Result<Self> {
        if providers.is_empty() {
            return Err(Error::InvalidConfig(
                "failover provider requires at least one child provider".into(),
            ));
        }
        Ok(Self { providers })
    }

    /// Returns the ordered child providers.
    pub fn providers(&self) -> &[Arc<dyn UpdateProvider>] {
        &self.providers
    }

    fn resolve_child<'a>(
        &'a self,
        id: &ArtifactId,
    ) -> Result<(&'a Arc<dyn UpdateProvider>, ArtifactId)> {
        let Some((index, inner_id)) = id.pop_route() else {
            return Ok((&self.providers[0], id.clone()));
        };
        let provider = self.providers.get(index).ok_or_else(|| {
            Error::InvalidConfig(format!(
                "failover artifact route index {index} out of bounds"
            ))
        })?;
        Ok((provider, inner_id))
    }
}

impl UpdateProvider for FailoverProvider {
    fn resolve_release<'a>(
        &'a self,
        ctx: &'a ResolveContext,
    ) -> BoxFuture<'a, Result<Option<ResolvedRelease>>> {
        Box::pin(async move {
            let mut last_error: Option<Error> = None;
            for (index, provider) in self.providers.iter().enumerate() {
                match provider.resolve_release(ctx).await {
                    Ok(None) => return Ok(None),
                    Ok(Some(mut release)) => {
                        if ctx.require_checksum
                            && release
                                .sha512
                                .as_deref()
                                .is_none_or(|digest| digest.trim().is_empty())
                        {
                            let err = Error::InvalidManifest(
                                "manifest platform must provide non-empty `sha512` when `pubkey` is not configured".into(),
                            );
                            tracing::warn!(
                                provider_index = index,
                                error = %err,
                                "failover provider candidate rejected unverified release"
                            );
                            last_error = Some(err);
                            continue;
                        }
                        release.artifact = release.artifact.with_route(index);
                        release.blockmap = release.blockmap.map(|id| id.with_route(index));
                        return Ok(Some(release));
                    }
                    Err(error) => {
                        tracing::warn!(
                            provider_index = index,
                            error = %error,
                            "failover provider candidate failed"
                        );
                        last_error = Some(error);
                    }
                }
            }
            match last_error {
                Some(error) => Err(error),
                None => Ok(None),
            }
        })
    }

    fn fetch_blockmap<'a>(
        &'a self,
        id: &'a ArtifactId,
        max_bytes: usize,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            let (provider, inner_id) = self.resolve_child(id)?;
            provider
                .fetch_blockmap(&inner_id, max_bytes, overrides)
                .await
        })
    }

    fn open_session<'a>(
        &'a self,
        id: &'a ArtifactId,
        overrides: &'a TransferOverrides,
    ) -> BoxFuture<'a, Result<Box<dyn DownloadSession>>> {
        Box::pin(async move {
            let (provider, inner_id) = self.resolve_child(id)?;
            provider.open_session(&inner_id, overrides).await
        })
    }
}
