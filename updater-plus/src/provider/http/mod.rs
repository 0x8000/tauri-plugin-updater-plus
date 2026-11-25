//! Reusable HTTP downloader, middleware/authorizer hooks, and composed HTTP providers
//! (`StaticHttpProvider`, `GitHubProvider`, `SigV4Authorizer` / `S3Provider`).

mod builder;
mod downloader;
mod github;
mod middleware;
mod redirect;
mod session;
mod sigv4;
mod static_http;
#[cfg(test)]
mod tests;
mod wire;

pub use builder::HttpDownloaderBuilder;
pub use downloader::HttpDownloader;
pub use github::{GitHubProvider, GitHubProviderBuilder};
pub use middleware::{
    FnAuthorizer, HttpAuthorizer, HttpMiddleware, HttpNext, HttpRequest, HttpRequestContext,
    HttpResponse, RequestStage,
};
pub use session::HttpDownloadSession;
pub use sigv4::{
    AwsCredentials, AwsCredentialsProvider, FnCredentialsProvider, S3Provider, SigV4Authorizer,
    StaticAwsCredentials,
};
pub use static_http::StaticHttpProvider;
