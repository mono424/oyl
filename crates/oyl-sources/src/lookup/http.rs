//! A small blocking HTTP client: 15 s timeouts, a generic user agent, and
//! nothing about the person in any request.

use std::time::Duration;

use ureq::ResponseExt;

use crate::error::{Error, Result};

/// The user agent of every request. Generic on purpose: no e-mail, no user
/// name, no paths.
pub const USER_AGENT: &str = "Oyl/0.1";

/// How long a whole request may take.
const TIMEOUT: Duration = Duration::from_secs(15);

/// A fetched response.
pub(crate) struct Fetched {
    pub status: u16,
    /// The media type, lowercase, without parameters (`text/html`).
    pub mime: String,
    pub body: Vec<u8>,
    /// The URL after redirects.
    pub url: String,
}

impl Fetched {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The HTTP client lookups go through.
pub(crate) struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub(crate) fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(10)
            .build();
        Client {
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// GETs `url` with an `Accept` header and query parameters, reading at
    /// most `limit` bytes of body.
    pub(crate) fn get(
        &self,
        url: &str,
        accept: &str,
        query: &[(&str, &str)],
        limit: u64,
    ) -> Result<Fetched> {
        let mut request = self.agent.get(url).header("Accept", accept);
        for (k, v) in query {
            request = request.query(*k, *v);
        }
        let mut response = request.call().map_err(|e| network_error(url, e))?;
        let status = response.status().as_u16();
        let mime = response
            .body()
            .mime_type()
            .unwrap_or("")
            .to_ascii_lowercase();
        let final_url = response.get_uri().to_string();
        let body = response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| network_error(url, e))?;
        Ok(Fetched {
            status,
            mime,
            body,
            url: final_url,
        })
    }
}

fn network_error(url: &str, e: ureq::Error) -> Error {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(url);
    let reason = match &e {
        ureq::Error::Timeout(_) => "it took too long to answer".to_string(),
        ureq::Error::HostNotFound => {
            "the address could not be found (are you offline?)".to_string()
        }
        ureq::Error::Io(io) => format!("the connection failed ({io})"),
        ureq::Error::BodyExceedsLimit(_) => "the answer was too large".to_string(),
        other => other.to_string(),
    };
    Error::Network(format!("Could not reach {host}: {reason}."))
}
