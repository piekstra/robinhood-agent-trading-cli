//! OAuth 2.1 authorization-code flow with PKCE for the Robinhood Agentic MCP.
//!
//! Grounded in the server's own metadata (captured 2026-08-27, see
//! `docs/api.md`): PKCE `S256` is required, the client is **public**
//! (`token_endpoint_auth_methods_supported: ["none"]` — no client secret), the
//! authorization endpoint is `https://robinhood.com/oauth`, the token endpoint
//! is `https://api.robinhood.com/oauth2/token/`, and clients self-register via
//! RFC 7591 dynamic registration. Every value is resolved at runtime by
//! [`crate::discovery`]; nothing here hardcodes an endpoint.
//!
//! The pure pieces — PKCE, the authorization URL, callback parsing, token
//! parsing — are unit-tested offline. The browser round trip
//! ([`LoopbackListener`]) and the token/registration POSTs run only when the
//! owner invokes `rhap auth login`; they are never exercised by the test suite.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use base64::Engine as _;
use pk_cli_core::CliError;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{CLIENT_NAME, SCOPE};
use crate::credential::TokenSet;

/// A PKCE verifier/challenge pair (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh high-entropy verifier and its S256 challenge.
    pub fn generate() -> Self {
        let verifier = random_urlsafe(64);
        let challenge = s256_challenge(&verifier);
        Pkce {
            verifier,
            challenge,
        }
    }
}

/// `base64url(sha256(verifier))`, unpadded — the S256 code challenge.
pub fn s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// A URL-safe random token of `bytes` entropy, base64url-unpadded (used for the
/// PKCE verifier and the CSRF `state`).
pub fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// Build the authorization URL the owner opens in a browser.
///
/// `resource` is the RFC 8707 resource indicator (the MCP endpoint); the MCP
/// authorization spec requires it so the issued token is bound to this
/// resource.
#[allow(clippy::too_many_arguments)]
pub fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scope: &str,
    resource: &str,
) -> String {
    let q = query_string(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("scope", scope),
        ("resource", resource),
    ]);
    let sep = if authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{}{sep}{q}", authorization_endpoint.trim_end_matches('/'))
}

/// Extract and validate `(code)` from the browser's callback request-target.
///
/// Accepts the raw request target the loopback listener reads
/// (`/callback?code=…&state=…`), a full redirect URL, or a bare query string.
/// The `state` must byte-match `expected_state` or the request is rejected as a
/// possible CSRF — the whole point of carrying `state`.
pub fn code_from_callback(target: &str, expected_state: &str) -> Result<String, CliError> {
    let target = target.trim();
    if let Some(err) = query_param(target, "error") {
        let desc = query_param(target, "error_description").unwrap_or_default();
        return Err(CliError::Auth(format!(
            "authorization was refused ({err}{}{desc})",
            if desc.is_empty() { "" } else { ": " }
        )));
    }
    let state = query_param(target, "state").unwrap_or_default();
    if state != expected_state {
        return Err(CliError::Auth(
            "the OAuth `state` did not match — refusing the callback as a possible CSRF; \
             run `rhap auth login` again"
                .into(),
        ));
    }
    match query_param(target, "code") {
        Some(c) if !c.is_empty() => Ok(c),
        _ => Err(CliError::Auth(
            "the callback carried no authorization `code`".into(),
        )),
    }
}

/// RFC 7591 dynamic client registration: register the loopback redirect and
/// return the issued **public** client id.
pub fn register_client(
    http: &reqwest::blocking::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<String, CliError> {
    let body = serde_json::json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [redirect_uri],
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": SCOPE,
    });
    let resp = http
        .post(registration_endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .map_err(|e| CliError::Upstream(format!("client registration request failed: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(CliError::Upstream(format!(
            "client registration returned HTTP {status}: {text}"
        )));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| CliError::Upstream(format!("registration response is not JSON: {e}")))?;
    v.get("client_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| CliError::Upstream("registration response carried no `client_id`".into()))
}

/// Exchange an authorization code for a token set. Public client: `client_id`
/// travels in the body, there is no client secret, and the PKCE `code_verifier`
/// proves possession.
#[allow(clippy::too_many_arguments)]
pub fn exchange_code(
    http: &reqwest::blocking::Client,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    resource: &str,
    now: u64,
) -> Result<TokenSet, CliError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", verifier),
        ("resource", resource),
    ];
    let body = post_token(http, token_endpoint, &form)?;
    TokenSet::from_response(&body, now, None)
}

/// A client for what carries a credential: the bearer token, or a code or refresh token in a body
/// that reqwest re-sends on a 307 or 308. It follows a redirect only on the same host, over https.
pub fn credential_client() -> Result<reqwest::blocking::Client, CliError> {
    pk_cli_http::builder("rhap", env!("CARGO_PKG_VERSION"))
        .redirect(pk_cli_core::transport::same_host_redirects())
        .build()
        .map_err(|e| CliError::Other(format!("failed to build HTTP client: {e}")))
}

/// A client for the discovery documents, which carry no credential and may move host, though never
/// off https. `discovery::check_trusted_url` holds what they name to Robinhood's domain.
pub fn discovery_client() -> Result<reqwest::blocking::Client, CliError> {
    pk_cli_http::builder("rhap", env!("CARGO_PKG_VERSION"))
        .redirect(pk_cli_core::transport::secure_redirects())
        .build()
        .map_err(|e| CliError::Other(format!("failed to build HTTP client: {e}")))
}

/// Trade a refresh token for a fresh access token (public client).
pub fn refresh_tokens(
    http: &reqwest::blocking::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
    resource: &str,
    now: u64,
) -> Result<TokenSet, CliError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("scope", SCOPE),
        ("resource", resource),
    ];
    let body = post_token(http, token_endpoint, &form)?;
    TokenSet::from_response(&body, now, Some(refresh_token))
}

fn post_token(
    http: &reqwest::blocking::Client,
    token_endpoint: &str,
    form: &[(&str, &str)],
) -> Result<Value, CliError> {
    let resp = http
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .form(form)
        .send()
        .map_err(|e| CliError::Upstream(format!("token request failed: {e}")))?;
    token_response(resp)
}

/// Map the token endpoint's answer onto the family exit-code contract.
fn token_response(resp: reqwest::blocking::Response) -> Result<Value, CliError> {
    let status = resp.status().as_u16();
    let body = resp.text().unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&body).ok();

    if (200..300).contains(&status) {
        return parsed.ok_or_else(|| {
            CliError::Upstream(format!(
                "the token endpoint returned HTTP {status} with a non-JSON body"
            ))
        });
    }
    let slug = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = parsed
        .as_ref()
        .and_then(|v| v.get("error_description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(CliError::Auth(explain_oauth_error(slug, detail, status)))
}

/// Turn an OAuth error slug into an actionable sentence.
pub fn explain_oauth_error(slug: &str, detail: &str, status: u16) -> String {
    let base = match slug {
        "invalid_grant" => {
            "the authorization code or refresh token was rejected. If this was `auth login`, the \
             code is single-use and short-lived — run `rhap auth login` for a fresh one. If this \
             was an automatic refresh, the refresh token is spent — run `rhap auth login` to \
             authorize again."
        }
        "invalid_client" => {
            "the OAuth client was rejected. Re-register by running `rhap auth login` (dynamic \
             client registration issues a fresh client id)."
        }
        "invalid_request" => {
            "the token request was malformed or missing PKCE — this is a bug; see docs/api.md."
        }
        _ => "the token endpoint refused the request.",
    };
    match (slug.is_empty(), detail.is_empty()) {
        (false, false) => format!("{base} [{slug}: {detail}] (HTTP {status})"),
        (false, true) => format!("{base} [{slug}] (HTTP {status})"),
        (true, false) => format!("{base} [{detail}] (HTTP {status})"),
        (true, true) => format!("{base} (HTTP {status})"),
    }
}

// --------------------------------------------------------------- loopback

/// A one-shot loopback HTTP listener for the OAuth redirect (RFC 8252 §7.3).
pub struct LoopbackListener {
    listener: TcpListener,
    port: u16,
}

impl LoopbackListener {
    /// Bind `127.0.0.1:port` (`port = 0` picks a free ephemeral port).
    pub fn bind(port: u16) -> Result<Self, CliError> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| CliError::Other(format!("could not bind the loopback listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| CliError::Other(format!("reading loopback address: {e}")))?
            .port();
        Ok(LoopbackListener { listener, port })
    }

    /// The redirect URI to register and send in the authorization request.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Block on a single connection — the browser hitting the callback — and
    /// return the validated authorization code. Serves a small confirmation
    /// page so the browser tab doesn't hang.
    pub fn wait_for_code(&self, expected_state: &str) -> Result<String, CliError> {
        let (mut stream, _addr) = self
            .listener
            .accept()
            .map_err(|e| CliError::Other(format!("accepting the callback: {e}")))?;
        let target = read_request_target(&mut stream)?;
        let result = code_from_callback(&target, expected_state);
        respond(&mut stream, result.is_ok());
        result
    }
}

/// Read the request target (the path+query) out of the first request line.
fn read_request_target(stream: &mut TcpStream) -> Result<String, CliError> {
    let mut buf = [0u8; 4096];
    let n = stream
        .read(&mut buf)
        .map_err(|e| CliError::Other(format!("reading the callback request: {e}")))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    // `GET /callback?code=…&state=… HTTP/1.1`
    let line = head.lines().next().unwrap_or_default();
    line.split_whitespace()
        .nth(1)
        .map(str::to_string)
        .ok_or_else(|| CliError::Other("malformed callback request line".into()))
}

/// Serve a minimal confirmation page so the browser tab doesn't hang.
fn respond(stream: &mut TcpStream, ok: bool) {
    let msg = if ok {
        "rhap: authorization received. You can close this tab and return to the terminal."
    } else {
        "rhap: authorization failed. Return to the terminal for details."
    };
    let body = format!("<!doctype html><meta charset=utf-8><title>rhap</title><p>{msg}</p>");
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Best-effort browser launch. A failure is a convenience lost, not an error —
/// the URL was already printed.
pub fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let launcher = Some("open");
    #[cfg(target_os = "linux")]
    let launcher = Some("xdg-open");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let launcher: Option<&str> = None;

    match launcher {
        Some(cmd) => std::process::Command::new(cmd).arg(url).status().is_ok(),
        None => false,
    }
}

// ------------------------------------------------------------ url helpers

/// Percent-encode a query component, escaping everything but the RFC 3986
/// unreserved set.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build an `a=b&c=d` query string from encoded pairs.
pub fn query_string(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Pull a single decoded query parameter out of a URL or query string.
pub fn query_param(input: &str, key: &str) -> Option<String> {
    let qs = input.split_once('?').map(|(_, q)| q).unwrap_or(input);
    for pair in qs.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(decode(v));
            }
        } else if pair == key {
            return Some(String::new());
        }
    }
    None
}

/// Percent-decode once (`+` is treated literally; the query uses `%20`).
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that answers every request with `status` and `location`, and counts them.
    fn stub(status: u16, location: String) -> (String, std::sync::Arc<std::sync::Mutex<u32>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(0));
        let count = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                *count.lock().unwrap() += 1;
                let answer = format!(
                    "HTTP/1.1 {status} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                );
                let _ = conn.write_all(answer.as_bytes());
            }
        });
        (url, seen)
    }

    /// reqwest re-sends a form on a 307, so a refresh token must not follow one to another host.
    #[test]
    fn a_refresh_token_does_not_follow_a_redirect_to_another_host() {
        let (elsewhere, reached) = stub(500, String::new());
        let (token_endpoint, asked) = stub(307, elsewhere);
        let e = refresh_tokens(
            &credential_client().unwrap(),
            &token_endpoint,
            "dummy-client",
            "dummy-refresh-token",
            "https://example.invalid/resource",
            0,
        )
        .unwrap_err();
        assert!(e.message().to_lowercase().contains("redirect"), "{e}");
        assert_eq!(*asked.lock().unwrap(), 1);
        assert_eq!(*reached.lock().unwrap(), 0);
    }

    /// RFC 7636 Appendix B test vector: the canonical verifier→challenge pair.
    #[test]
    fn s256_challenge_matches_the_rfc_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            s256_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_generated_verifier_is_url_safe_and_high_entropy() {
        let p = Pkce::generate();
        assert!(p.verifier.len() >= 43, "RFC 7636 minimum length");
        assert!(p
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(p.challenge, s256_challenge(&p.verifier));
        // Two draws must differ.
        assert_ne!(Pkce::generate().verifier, Pkce::generate().verifier);
    }

    #[test]
    fn the_authorize_url_carries_pkce_and_the_resource_indicator() {
        let url = authorize_url(
            "https://robinhood.com/oauth",
            "CLIENT123",
            "http://127.0.0.1:52321/callback",
            "CHALLENGE",
            "STATE",
            "internal",
            "https://agent.robinhood.com/mcp/trading",
        );
        assert!(url.starts_with("https://robinhood.com/oauth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=CLIENT123"));
        assert!(url.contains("code_challenge=CHALLENGE"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=STATE"));
        assert!(url.contains("scope=internal"));
        // redirect_uri and resource must be percent-encoded.
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A52321%2Fcallback"));
        assert!(url.contains("resource=https%3A%2F%2Fagent.robinhood.com%2Fmcp%2Ftrading"));
    }

    #[test]
    fn the_callback_yields_the_code_when_state_matches() {
        let target = "/callback?code=abc123&state=XYZ";
        assert_eq!(code_from_callback(target, "XYZ").unwrap(), "abc123");
    }

    #[test]
    fn a_state_mismatch_is_rejected_as_csrf() {
        let err = code_from_callback("/callback?code=abc&state=WRONG", "XYZ").unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("CSRF"));
    }

    #[test]
    fn an_error_callback_surfaces_the_provider_message() {
        let err = code_from_callback(
            "/callback?error=access_denied&error_description=User%20denied&state=XYZ",
            "XYZ",
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("access_denied"));
        assert!(err.to_string().contains("User denied"));
    }

    #[test]
    fn a_codeless_callback_is_an_auth_error() {
        let err = code_from_callback("/callback?state=XYZ", "XYZ").unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("code"));
    }

    #[test]
    fn percent_coding_round_trips() {
        let raw = "http://127.0.0.1:8080/callback?x=a b";
        assert_eq!(decode(&encode(raw)), raw);
        assert_eq!(encode("a/b:c"), "a%2Fb%3Ac");
    }

    #[test]
    fn oauth_error_messages_are_actionable() {
        let msg = explain_oauth_error("invalid_grant", "expired", 400);
        assert!(msg.contains("single-use") || msg.contains("refresh"));
        assert!(msg.contains("invalid_grant"));
        assert!(msg.contains("expired"));
        assert!(explain_oauth_error("", "", 500).contains("HTTP 500"));
    }
}
