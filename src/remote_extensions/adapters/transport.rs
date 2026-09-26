use super::AdapterExecutionError;
use crate::remote_extensions::public_ip;
use reqwest::Client;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use url::{Host, Url};

/// Resolve immediately before dispatch and pin the accepted address in reqwest.
/// This prevents a public DNS name from switching to a private address between
/// catalog validation and the actual HTTP connection.
pub(crate) async fn client_for_endpoint(
    endpoint: &str,
    timeout: Duration,
    allow_local_for_testing: bool,
) -> Result<Client, AdapterExecutionError> {
    let url = Url::parse(endpoint)
        .map_err(|_| AdapterExecutionError::Network("invalid endpoint URL".into()))?;
    if url.scheme() != "https" && !(allow_local_for_testing && url.scheme() == "http") {
        return Err(AdapterExecutionError::Network(
            "endpoint must use HTTPS".into(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(AdapterExecutionError::Network(
            "endpoint URL contains forbidden credentials or fragment".into(),
        ));
    }
    let host = url
        .host()
        .ok_or_else(|| AdapterExecutionError::Network("endpoint has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AdapterExecutionError::Network("endpoint has no port".into()))?;
    let mut builder = Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy();
    match host {
        Host::Domain(domain) => {
            if (domain.eq_ignore_ascii_case("localhost")
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
                || domain.ends_with(".internal"))
                && !allow_local_for_testing
            {
                return Err(AdapterExecutionError::Network(
                    "local endpoint is forbidden".into(),
                ));
            }
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|_| AdapterExecutionError::Network("endpoint DNS lookup failed".into()))?
                .collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|addr| !allowed_ip(addr.ip(), allow_local_for_testing))
            {
                return Err(AdapterExecutionError::Network(
                    "endpoint resolves to a forbidden network address".into(),
                ));
            }
            builder = builder.resolve(domain, addresses[0]);
        }
        Host::Ipv4(ip) => {
            if !allowed_ip(IpAddr::V4(ip), allow_local_for_testing) {
                return Err(AdapterExecutionError::Network(
                    "endpoint uses a forbidden network address".into(),
                ));
            }
        }
        Host::Ipv6(ip) => {
            if !allowed_ip(IpAddr::V6(ip), allow_local_for_testing) {
                return Err(AdapterExecutionError::Network(
                    "endpoint uses a forbidden network address".into(),
                ));
            }
        }
    }
    builder
        .build()
        .map_err(|_| AdapterExecutionError::Network("HTTP transport initialization failed".into()))
}

fn allowed_ip(ip: IpAddr, allow_local_for_testing: bool) -> bool {
    public_ip(ip) || (allow_local_for_testing && ip.is_loopback())
}
