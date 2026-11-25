mod blockmap;
#[cfg(feature = "engine")]
mod cache;
mod chunker;
#[cfg(feature = "engine")]
mod config;
#[cfg(feature = "engine")]
mod downloader;
mod error;
#[cfg(feature = "engine")]
mod installer;
#[cfg(feature = "engine")]
mod listener;
mod manifest;
#[cfg(feature = "engine")]
mod multipart;
mod planner;
#[cfg(feature = "engine")]
mod provider;
#[cfg(feature = "engine")]
mod session;
#[cfg(feature = "engine")]
mod updater;
#[cfg(feature = "engine")]
mod verifier;

pub use blockmap::{BlockMap, BlockMapArtifact, BlockMapFile};
#[cfg(feature = "engine")]
pub use cache::{CachedPackageState, LocalBasePackage, PackageCache};
pub use chunker::{ChunkerConfig, FastCdc};
#[cfg(feature = "engine")]
pub use config::{Config, WindowsConfig, WindowsInstallMode};
#[cfg(feature = "http")]
pub use downloader::DownloadRequest;
#[cfg(feature = "engine")]
pub use downloader::{DifferentialDownloader, ProviderDownloadRequest, TransferOptions};
pub use error::{Error, Result, TransportError, TransportErrorKind};
#[cfg(feature = "native-installer")]
pub use installer::{AndroidApkLauncher, AndroidInstaller, DesktopInstaller, PlatformInstaller};
#[cfg(feature = "engine")]
pub use installer::{
    AuthenticodePolicy, InstallContext, MacosCodeSignVerifier, MacosSigningRequirement,
    UnsupportedInstaller, UpdateInstaller, WindowsAuthenticodeVerifier,
};
#[cfg(feature = "engine")]
pub use listener::{DownloadEvent, DownloadListener, DownloadMode, DownloadSummary, DummyListener};
pub use manifest::{ReleaseManifest, ReleasePlatform, ReleasePlatforms};
#[cfg(feature = "engine")]
pub use multipart::{ByteRangeParser, ByteRangePart};
pub use planner::{ByteRange, DownloadPlan, PlanOperation, PlanOptions};
#[cfg(feature = "http")]
pub use provider::http::{
    AwsCredentials, AwsCredentialsProvider, FnAuthorizer, FnCredentialsProvider, GitHubProvider,
    GitHubProviderBuilder, HttpAuthorizer, HttpDownloadSession, HttpDownloader,
    HttpDownloaderBuilder, HttpMiddleware, HttpNext, HttpRequest, HttpRequestContext, HttpResponse,
    RequestStage, S3Provider, SigV4Authorizer, StaticAwsCredentials, StaticHttpProvider,
};
#[cfg(feature = "engine")]
pub use provider::{
    ArtifactId, BoxByteStream, BoxFuture, ByteStream, DownloadSession, FailoverProvider,
    RangeCapability, RangeFetchResult, RangePart, ResolveContext, ResolvedRelease,
    TransferCapabilities, TransferOverrides, TransferStats, UpdateProvider,
};
#[cfg(feature = "engine")]
pub use session::{DiffPlanInfo, DiffSession, DownloadedPackage, FullSession};
#[cfg(feature = "engine")]
pub use updater::{TargetResolver, Update, Updater, UpdaterBuilder};
#[cfg(feature = "engine")]
pub use verifier::{ArtifactVerifier, VerifyExpectation, VerifyPolicy};
