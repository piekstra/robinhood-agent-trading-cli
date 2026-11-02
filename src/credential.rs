//! The one secret this CLI holds — the OAuth token bundle — and how it is
//! parsed, persisted, and kept out of logs.
//!
//! The access token (short-lived) and refresh token are obtained by `rhap auth
//! login` and rewritten every time the access token is refreshed. Both live in
//! the OS keychain under `piekstra.rhap`, in a single `tokens` entry.
//!
//! [`TokenSet`] implements [`std::fmt::Debug`] **by hand** so that a stray
//! `{:?}` in a future debug session cannot print a bearer token to a terminal
//! or a log file.

use pk_cli_core::CliError;
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::KEYCHAIN_TOKENS;

/// Treat an access token expiring within this many seconds as already gone, so
/// a command doesn't start work that will 401 partway through.
pub const EXPIRY_SKEW_SECS: u64 = 60;

/// An OAuth token set as persisted between runs.
///
/// `expires_at` is an absolute Unix second rather than the `expires_in`
/// duration the token endpoint returns, because a duration is meaningless once
/// it has been written to the keychain and read back an hour later.
///
/// Robinhood's refresh-token lifetime is not documented and not carried in the
/// token response, so — unlike the Schwab CLI — no local refresh deadline is
/// fabricated. A refresh that the server rejects surfaces as an ordinary auth
/// error pointing at `rhap auth login`.
#[derive(Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token stops working (Unix seconds).
    pub expires_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish()
    }
}

impl TokenSet {
    pub fn access_expired(&self, now: u64) -> bool {
        self.expires_at.saturating_sub(EXPIRY_SKEW_SECS) <= now
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn parse(raw: &str) -> Result<Self, CliError> {
        serde_json::from_str(raw).map_err(|e| {
            CliError::Auth(format!(
                "stored token set is unreadable ({e}) — run `rhap auth login`"
            ))
        })
    }

    /// Build a [`TokenSet`] from an OAuth token-endpoint response body.
    ///
    /// A missing or unreadable `expires_in` is treated as a very short life
    /// rather than a long one: being wrong in that direction costs one extra
    /// refresh, while the other direction serves a dead token and produces a
    /// mystery 401. A refresh response that omits a new `refresh_token` keeps
    /// the one passed as `prior_refresh` (some servers rotate it, some don't).
    pub fn from_response(
        body: &Value,
        now: u64,
        prior_refresh: Option<&str>,
    ) -> Result<Self, CliError> {
        let access_token = str_field(body, "access_token")?;
        let refresh_token = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| prior_refresh.map(str::to_string))
            .ok_or_else(|| {
                CliError::Upstream(
                    "token response has no `refresh_token` and none was carried forward — \
                     the response shape has changed; see docs/api.md"
                        .into(),
                )
            })?;
        let expires_in = body.get("expires_in").and_then(Value::as_u64).unwrap_or(60);
        Ok(TokenSet {
            access_token,
            refresh_token,
            expires_at: now + expires_in,
            scope: body
                .get("scope")
                .and_then(Value::as_str)
                .map(str::to_string),
            token_type: body
                .get("token_type")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

fn str_field(body: &Value, name: &str) -> Result<String, CliError> {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Upstream(format!(
                "token response has no `{name}` — the response shape has changed; see docs/api.md"
            ))
        })
}

pub fn read_tokens(creds: &CredentialStore) -> Result<Option<TokenSet>, CliError> {
    match creds.get(KEYCHAIN_TOKENS)? {
        Some(s) => Ok(Some(TokenSet::parse(s.expose())?)),
        None => Ok(None),
    }
}

pub fn write_tokens(creds: &CredentialStore, tokens: &TokenSet) -> Result<(), CliError> {
    creds.set(KEYCHAIN_TOKENS, &Secret::new(tokens.to_json()))
}

/// Look at the keychain without failing when it is unreachable.
///
/// SPEC v1 §1.2 requires `auth status` to work logged out, and that has to
/// include the case where the keychain itself refuses. Returns the note to show
/// the user alongside the DTO.
pub fn peek(creds: &CredentialStore) -> (Option<Secret>, Option<String>) {
    match creds.get(KEYCHAIN_TOKENS) {
        Ok(v) => (v, None),
        Err(e) => (None, Some(format!("could not read the keychain: {e}"))),
    }
}

/// Current Unix time in seconds.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_token_response_becomes_an_absolute_deadline() {
        let body = json!({
            "access_token": "AT", "refresh_token": "RT",
            "expires_in": 1800, "token_type": "Bearer", "scope": "internal"
        });
        let t = TokenSet::from_response(&body, 1_000, None).unwrap();
        assert_eq!(t.expires_at, 2_800);
        assert_eq!(t.scope.as_deref(), Some("internal"));
        assert_eq!(t.token_type.as_deref(), Some("Bearer"));
    }

    #[test]
    fn a_refresh_without_a_new_refresh_token_keeps_the_old_one() {
        let body = json!({"access_token": "AT2", "expires_in": 1800});
        let t = TokenSet::from_response(&body, 1_000, Some("RT-ORIGINAL")).unwrap();
        assert_eq!(t.refresh_token, "RT-ORIGINAL");
        // A rotated refresh token, when present, wins over the carried-forward one.
        let rotated = json!({"access_token": "AT3", "refresh_token": "RT-NEW", "expires_in": 60});
        let t = TokenSet::from_response(&rotated, 0, Some("RT-ORIGINAL")).unwrap();
        assert_eq!(t.refresh_token, "RT-NEW");
    }

    #[test]
    fn a_missing_expires_in_is_treated_as_a_short_life() {
        let body = json!({"access_token": "AT", "refresh_token": "RT"});
        let t = TokenSet::from_response(&body, 1_000, None).unwrap();
        assert!(t.expires_at - 1_000 <= 60);
    }

    #[test]
    fn a_token_response_missing_the_access_token_is_an_upstream_error() {
        let err = TokenSet::from_response(&json!({"refresh_token": "RT"}), 0, None).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("access_token"));
    }

    #[test]
    fn expiry_uses_the_skew_so_a_doomed_request_is_not_started() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1_000,
            scope: None,
            token_type: None,
        };
        assert!(!t.access_expired(1_000 - EXPIRY_SKEW_SECS - 1));
        assert!(t.access_expired(1_000 - EXPIRY_SKEW_SECS));
        assert!(t.access_expired(2_000));
    }

    #[test]
    fn debug_output_never_contains_token_material() {
        let t = TokenSet {
            access_token: "ACCESSTOKENVALUE".into(),
            refresh_token: "REFRESHTOKENVALUE".into(),
            expires_at: 100,
            scope: None,
            token_type: None,
        };
        let shown = format!("{t:?}");
        assert!(!shown.contains("ACCESSTOKENVALUE"), "{shown}");
        assert!(!shown.contains("REFRESHTOKENVALUE"), "{shown}");
    }

    #[test]
    fn an_unreadable_token_set_is_an_auth_error_pointing_at_login() {
        let err = TokenSet::parse("not json").unwrap_err();
        assert_eq!(err.exit_code(), 3);
        assert!(err.to_string().contains("auth login"));
    }

    #[test]
    fn a_token_set_round_trips_through_json() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 42,
            scope: Some("internal".into()),
            token_type: Some("Bearer".into()),
        };
        let back = TokenSet::parse(&t.to_json()).unwrap();
        assert_eq!(back.refresh_token, "r");
        assert_eq!(back.expires_at, 42);
        assert_eq!(back.scope.as_deref(), Some("internal"));
    }
}
