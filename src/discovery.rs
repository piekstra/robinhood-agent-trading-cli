//! OAuth 2.1 server-metadata discovery for the MCP endpoint.
//!
//! The MCP spec has clients discover the authorization server rather than
//! hardcoding endpoints, in two hops:
//!
//! 1. **RFC 9728** — GET the protected resource's
//!    `/.well-known/oauth-protected-resource`. Its `authorization_servers`
//!    lists the issuer(s) that guard the resource.
//! 2. **RFC 8414** — GET the issuer's `/.well-known/oauth-authorization-server`
//!    for the `authorization_endpoint`, `token_endpoint`, and
//!    `registration_endpoint`.
//!
//! Fetching at runtime means a Robinhood-side endpoint move is picked up
//! without a release. The parsing below is pure and unit-tested against the
//! captured well-known documents in `tests/fixtures/`; only [`discover`] touches
//! the network.

use pk_cli_core::CliError;
use serde::Deserialize;

use crate::config::{MCP_ENDPOINT, PROTECTED_RESOURCE_METADATA};

/// The subset of RFC 9728 protected-resource metadata this CLI reads.
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResource {
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
}

/// The subset of RFC 8414 authorization-server metadata this CLI reads.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthServer {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// Everything a login/refresh needs, resolved from the two well-known docs.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    /// True when the server advertises PKCE with S256 (the only method this CLI
    /// will use). The MCP endpoint does; the check exists so a server that
    /// dropped it fails loudly rather than sending a plain `code`.
    pub supports_s256: bool,
}

impl AuthServer {
    fn into_endpoints(self) -> Endpoints {
        let supports_s256 = self
            .code_challenge_methods_supported
            .iter()
            .any(|m| m.eq_ignore_ascii_case("S256"));
        Endpoints {
            issuer: self.issuer,
            authorization_endpoint: self.authorization_endpoint,
            token_endpoint: self.token_endpoint,
            registration_endpoint: self.registration_endpoint,
            supports_s256,
        }
    }
}

impl ProtectedResource {
    /// The first advertised authorization server, or an error if none.
    pub fn first_auth_server(&self) -> Result<&str, CliError> {
        self.authorization_servers
            .first()
            .map(String::as_str)
            .ok_or_else(|| {
                CliError::Upstream(
                    "the protected-resource metadata lists no authorization_servers — \
                 cannot discover the OAuth endpoints (see docs/api.md)"
                        .into(),
                )
            })
    }
}

/// The registrable domain every discovered URL must sit under.
const TRUSTED_DOMAIN: &str = "robinhood.com";

/// Require `url` to be `https` on `robinhood.com` or a subdomain of it, with
/// no userinfo and the default port.
///
/// Discovery used to take whatever the well-known documents said, so a
/// tampered or spoofed document could name any token endpoint — and the
/// refresh token would be posted to it (audit 2026-09-23, security #7).
pub fn check_trusted_url(field: &str, url: &str) -> Result<(), CliError> {
    let refuse = |why: &str| {
        CliError::Upstream(format!(
            "discovery: {field} `{url}` {why} — refusing to send credentials there \
             (see docs/api.md)"
        ))
    };
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse("is not a valid URL"))?;
    if parsed.scheme() != "https" {
        return Err(refuse("is not https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refuse("carries userinfo"));
    }
    if parsed.port().is_some() {
        return Err(refuse("names a non-default port"));
    }
    // `domain()` is `None` for an IP-literal host.
    let host = parsed
        .domain()
        .ok_or_else(|| refuse("has no domain host"))?
        .to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    if host != TRUSTED_DOMAIN && !host.ends_with(&format!(".{TRUSTED_DOMAIN}")) {
        return Err(refuse("is not on robinhood.com"));
    }
    Ok(())
}

/// Validate the authorization-server metadata against the issuer the
/// protected-resource document named (`authorization_servers[0]`): the
/// metadata's `issuer` must equal it (RFC 8414 §3.3), and it and every
/// endpoint must pass [`check_trusted_url`].
pub fn validate_endpoints(expected_issuer: &str, ep: &Endpoints) -> Result<(), CliError> {
    check_trusted_url("issuer", expected_issuer)?;
    if ep.issuer.trim_end_matches('/') != expected_issuer.trim_end_matches('/') {
        return Err(CliError::Upstream(format!(
            "discovery: authorization-server metadata names issuer `{}`, not `{expected_issuer}` \
             from authorization_servers[0] — refusing to continue (see docs/api.md)",
            ep.issuer
        )));
    }
    check_trusted_url("authorization_endpoint", &ep.authorization_endpoint)?;
    check_trusted_url("token_endpoint", &ep.token_endpoint)?;
    if let Some(reg) = &ep.registration_endpoint {
        check_trusted_url("registration_endpoint", reg)?;
    }
    Ok(())
}

/// Candidate RFC 8414 metadata URLs for an issuer, in the order to try.
///
/// RFC 8414 inserts the well-known segment *before* the issuer path
/// (`https://host/.well-known/oauth-authorization-server/mcp/trading`); many
/// servers also answer at the path-suffixed form
/// (`https://host/mcp/trading/.well-known/...`). Robinhood's issuer
/// (`https://agent.robinhood.com/mcp/trading`) is served at the host root, which
/// the first candidate covers. Trying both keeps discovery working if that
/// moves.
pub fn auth_server_metadata_urls(issuer: &str) -> Vec<String> {
    let issuer = issuer.trim_end_matches('/');
    let mut urls = Vec::new();
    if let Some((origin, path)) = split_origin_path(issuer) {
        // RFC 8414: well-known segment before the path component.
        if path.is_empty() {
            urls.push(format!("{origin}/.well-known/oauth-authorization-server"));
        } else {
            urls.push(format!(
                "{origin}/.well-known/oauth-authorization-server{path}"
            ));
            // OIDC-style and path-suffixed fallbacks.
            urls.push(format!("{origin}/.well-known/openid-configuration{path}"));
            urls.push(format!("{issuer}/.well-known/oauth-authorization-server"));
        }
    } else {
        urls.push(format!("{issuer}/.well-known/oauth-authorization-server"));
    }
    urls
}

/// Split `https://host/a/b` into (`https://host`, `/a/b`); path is `""` for a
/// bare origin.
fn split_origin_path(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let scheme = if url.starts_with("https://") {
        "https://"
    } else {
        "http://"
    };
    match rest.find('/') {
        Some(i) => Some((format!("{scheme}{}", &rest[..i]), rest[i..].to_string())),
        None => Some((format!("{scheme}{rest}"), String::new())),
    }
}

/// Discover the OAuth endpoints for the MCP resource. Network side; the parsing
/// it delegates to is unit-tested offline.
pub fn discover(http: &reqwest::blocking::Client) -> Result<Endpoints, CliError> {
    let pr: ProtectedResource = get_json(http, PROTECTED_RESOURCE_METADATA)?;
    // Defensive: the discovery document must describe the resource we intend to
    // reach. A mismatch means the metadata was tampered with or moved, and we
    // must not bind a token to a different resource.
    if let Some(resource) = &pr.resource {
        if resource.trim_end_matches('/') != MCP_ENDPOINT.trim_end_matches('/') {
            return Err(CliError::Upstream(format!(
                "protected-resource metadata describes `{resource}`, not the expected \
                 `{MCP_ENDPOINT}` — refusing to continue (see docs/api.md)"
            )));
        }
    }
    let issuer = pr.first_auth_server()?.to_string();
    // Before any request to it: the metadata URLs are built from the issuer.
    check_trusted_url("issuer", &issuer)?;

    let mut last_err = None;
    for url in auth_server_metadata_urls(&issuer) {
        match get_json::<AuthServer>(http, &url) {
            Ok(md) => {
                let ep = md.into_endpoints();
                validate_endpoints(&issuer, &ep)?;
                if !ep.supports_s256 {
                    return Err(CliError::Upstream(format!(
                        "authorization server {} does not advertise PKCE S256; refusing to send \
                         a plain authorization code (see docs/api.md)",
                        ep.issuer
                    )));
                }
                return Ok(ep);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        CliError::Upstream(format!(
            "could not fetch authorization-server metadata for issuer {issuer}"
        ))
    }))
}

fn get_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::blocking::Client,
    url: &str,
) -> Result<T, CliError> {
    let resp = http
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| CliError::Upstream(format!("fetching {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(CliError::Upstream(format!(
            "{url} returned HTTP {}",
            resp.status().as_u16()
        )));
    }
    resp.json::<T>()
        .map_err(|e| CliError::Upstream(format!("parsing {url}: {e}")))
}

/// The MCP endpoint doubles as the RFC 8707 `resource` indicator on the
/// authorization and token requests.
pub fn resource_indicator() -> &'static str {
    MCP_ENDPOINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_resource_names_its_authorization_server() {
        let raw = r#"{
            "resource": "https://agent.robinhood.com/mcp/trading",
            "authorization_servers": ["https://agent.robinhood.com/mcp/trading"],
            "scopes_supported": ["internal"],
            "bearer_methods_supported": ["header"]
        }"#;
        let pr: ProtectedResource = serde_json::from_str(raw).unwrap();
        assert_eq!(
            pr.first_auth_server().unwrap(),
            "https://agent.robinhood.com/mcp/trading"
        );
        assert_eq!(
            pr.resource.as_deref(),
            Some("https://agent.robinhood.com/mcp/trading")
        );
    }

    #[test]
    fn no_authorization_servers_is_an_upstream_error() {
        let pr: ProtectedResource =
            serde_json::from_str(r#"{"resource":"x","authorization_servers":[]}"#).unwrap();
        assert_eq!(pr.first_auth_server().unwrap_err().exit_code(), 5);
    }

    #[test]
    fn authorization_server_metadata_parses_the_endpoints() {
        let raw = r#"{
            "issuer": "https://agent.robinhood.com/mcp/trading",
            "authorization_endpoint": "https://robinhood.com/oauth",
            "token_endpoint": "https://api.robinhood.com/oauth2/token/",
            "registration_endpoint": "https://agent.robinhood.com/oauth/trading/register",
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "response_types_supported": ["code"]
        }"#;
        let md: AuthServer = serde_json::from_str(raw).unwrap();
        let ep = md.into_endpoints();
        assert_eq!(ep.authorization_endpoint, "https://robinhood.com/oauth");
        assert_eq!(ep.token_endpoint, "https://api.robinhood.com/oauth2/token/");
        assert_eq!(
            ep.registration_endpoint.as_deref(),
            Some("https://agent.robinhood.com/oauth/trading/register")
        );
        assert!(ep.supports_s256, "the live server advertises S256");
    }

    #[test]
    fn a_server_without_s256_is_flagged() {
        let md = AuthServer {
            issuer: "i".into(),
            authorization_endpoint: "a".into(),
            token_endpoint: "t".into(),
            registration_endpoint: None,
            code_challenge_methods_supported: vec!["plain".into()],
        };
        assert!(!md.into_endpoints().supports_s256);
    }

    #[test]
    fn metadata_urls_put_the_well_known_segment_before_the_path() {
        let urls = auth_server_metadata_urls("https://agent.robinhood.com/mcp/trading");
        assert_eq!(
            urls[0],
            "https://agent.robinhood.com/.well-known/oauth-authorization-server/mcp/trading"
        );
        // A path-suffixed fallback is offered too.
        assert!(urls.iter().any(|u| u
            == "https://agent.robinhood.com/mcp/trading/.well-known/oauth-authorization-server"));
    }

    fn live_endpoints() -> Endpoints {
        Endpoints {
            issuer: "https://agent.robinhood.com/mcp/trading".into(),
            authorization_endpoint: "https://robinhood.com/oauth".into(),
            token_endpoint: "https://api.robinhood.com/oauth2/token/".into(),
            registration_endpoint: Some(
                "https://agent.robinhood.com/oauth/trading/register".into(),
            ),
            supports_s256: true,
        }
    }

    const ISSUER: &str = "https://agent.robinhood.com/mcp/trading";

    #[test]
    fn the_live_metadata_validates() {
        validate_endpoints(ISSUER, &live_endpoints()).unwrap();
        // A trailing slash on either side is the same issuer.
        validate_endpoints(&format!("{ISSUER}/"), &live_endpoints()).unwrap();
    }

    #[test]
    fn trusted_urls_are_https_on_robinhood_com() {
        for ok in [
            "https://robinhood.com/oauth",
            "https://api.robinhood.com/oauth2/token/",
            "https://a.b.robinhood.com/x",
            "https://API.Robinhood.com/x",
        ] {
            check_trusted_url("f", ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "http://api.robinhood.com/oauth2/token/",
            "https://evilrobinhood.com/token",
            "https://robinhood.com.evil.example/token",
            "https://api.robinhood.co/token",
            "https://attacker.example/token",
            "https://user:pw@api.robinhood.com/token",
            "https://api.robinhood.com:8443/token",
            "https://127.0.0.1/token",
            "https://[::1]/token",
            "ftp://api.robinhood.com/token",
            "api.robinhood.com/token",
            "",
        ] {
            let e = check_trusted_url("token_endpoint", bad).unwrap_err();
            assert_eq!(e.exit_code(), 5, "`{bad}` must be refused");
        }
    }

    #[test]
    fn each_endpoint_is_checked() {
        let mut ep = live_endpoints();
        ep.token_endpoint = "https://attacker.example/oauth2/token/".into();
        assert!(validate_endpoints(ISSUER, &ep).is_err());

        let mut ep = live_endpoints();
        ep.authorization_endpoint = "http://robinhood.com/oauth".into();
        assert!(validate_endpoints(ISSUER, &ep).is_err());

        let mut ep = live_endpoints();
        ep.registration_endpoint = Some("https://attacker.example/register".into());
        assert!(validate_endpoints(ISSUER, &ep).is_err());

        let mut ep = live_endpoints();
        ep.registration_endpoint = None;
        validate_endpoints(ISSUER, &ep).unwrap();
    }

    #[test]
    fn the_issuer_must_match_authorization_servers_0() {
        let mut ep = live_endpoints();
        ep.issuer = "https://api.robinhood.com/other".into();
        let e = validate_endpoints(ISSUER, &ep).unwrap_err();
        assert!(e.to_string().contains("authorization_servers[0]"), "{e}");
        // The expected issuer itself must be trusted.
        let mut ep = live_endpoints();
        ep.issuer = "http://agent.robinhood.com/mcp/trading".into();
        assert!(validate_endpoints("http://agent.robinhood.com/mcp/trading", &ep).is_err());
    }

    #[test]
    fn a_bare_origin_issuer_uses_the_root_well_known() {
        let urls = auth_server_metadata_urls("https://issuer.example.com");
        assert_eq!(
            urls[0],
            "https://issuer.example.com/.well-known/oauth-authorization-server"
        );
    }
}
