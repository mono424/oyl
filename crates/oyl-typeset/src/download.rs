//! HTTPS downloads: Typst Universe packages and the tectonic release.
//!
//! The same stack as typst-kit's `SystemDownloader` (ureq + native TLS,
//! proxies from the environment), but with connect and read timeouts so that
//! a stalled network cannot hang a compile on the background thread.

use std::any::Any;
use std::io::{self, Read};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use typst_kit::downloader::Downloader;

/// User agent sent with every request.
const USER_AGENT: &str = concat!("oyl/", env!("CARGO_PKG_VERSION"));

/// A lazily configured HTTPS client.
pub(crate) struct Http {
    agent: OnceLock<Result<ureq::Agent, String>>,
}

impl Http {
    /// A client; TLS is set up on the first request.
    pub(crate) fn new() -> Self {
        Self {
            agent: OnceLock::new(),
        }
    }

    fn agent(&self) -> io::Result<&ureq::Agent> {
        let agent = self.agent.get_or_init(|| {
            let tls = native_tls::TlsConnector::new().map_err(|err| err.to_string())?;
            Ok(ureq::AgentBuilder::new()
                .user_agent(USER_AGENT)
                .timeout_connect(Duration::from_secs(15))
                .timeout_read(Duration::from_secs(30))
                .try_proxy_from_env(true)
                .tls_connector(Arc::new(tls))
                .build())
        });
        agent
            .as_ref()
            .map_err(|err| io::Error::other(format!("TLS is unavailable: {err}")))
    }

    /// Starts a GET request and returns the size hint and the body.
    ///
    /// A 404 maps to [`io::ErrorKind::NotFound`], as typst-kit expects.
    pub(crate) fn get(
        &self,
        url: &str,
        accept: Option<&str>,
    ) -> io::Result<(Option<usize>, Box<dyn Read + Send + Sync>)> {
        let mut request = self.agent()?.get(url);
        if let Some(accept) = accept {
            request = request.set("Accept", accept);
        }
        let response = request.call().map_err(|err| match err {
            ureq::Error::Status(404, _) => {
                io::Error::new(io::ErrorKind::NotFound, format!("{url} was not found"))
            }
            ureq::Error::Status(code, response) => {
                io::Error::other(format!("{url} answered {code} {}", response.status_text()))
            }
            ureq::Error::Transport(transport) => io::Error::other(transport.to_string()),
        })?;
        let length = response
            .header("Content-Length")
            .and_then(|value| value.parse().ok());
        Ok((length, response.into_reader()))
    }
}

impl Downloader for Http {
    fn stream(&self, _key: &dyn Any, url: &str) -> io::Result<(Option<usize>, Box<dyn Read>)> {
        let (length, reader) = self.get(url, None)?;
        Ok((length, reader))
    }
}
