use std::path::PathBuf;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Minimal HTTP transport over a unix socket, mockable in tests.
#[async_trait]
pub(super) trait Transport: Send + Sync + 'static {
    async fn call(
        &self,
        method: &Method,
        path: &str,
        body: Option<Bytes>,
    ) -> Result<(u16, Bytes)>;
}

/// HTTP/1.1 client speaking to the Incus unix socket via `hyperlocal`.
pub(super) struct HttpUnixTransport {
    client: Client<hyperlocal::UnixConnector, Full<Bytes>>,
    socket: PathBuf,
}

impl HttpUnixTransport {
    pub(super) fn new(socket: impl Into<PathBuf>) -> Self {
        let client = Client::builder(TokioExecutor::new()).build(hyperlocal::UnixConnector);
        Self {
            client,
            socket: socket.into(),
        }
    }
}

#[async_trait]
impl Transport for HttpUnixTransport {
    async fn call(
        &self,
        method: &Method,
        path: &str,
        body: Option<Bytes>,
    ) -> Result<(u16, Bytes)> {
        let uri: hyper::Uri = hyperlocal::Uri::new(&self.socket, path).into();
        let mut builder = Request::builder().method(method.clone()).uri(uri);
        let request = if let Some(body) = body {
            builder = builder.header(http::header::CONTENT_TYPE, "application/json");
            builder.body(Full::new(body))
        } else {
            builder.body(Full::new(Bytes::new()))
        }
        .map_err(|error| Error::EngineUnavailable(error.to_string()))?;

        let response = self
            .client
            .request(request)
            .await
            .map_err(|error| Error::EngineUnavailable(format!("incus socket: {error}")))?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|error| Error::EngineUnavailable(format!("incus response: {error}")))?
            .to_bytes();
        Ok((status.as_u16(), bytes))
    }
}

