//! Constants and the non-secret on-disk settings.
//!
//! # What lives where
//!
//! Only non-secret values land on disk (`config.json`): the OAuth **client id**
//! issued by dynamic client registration. A registered client id is a *public*
//! identifier of the app, not a credential — this is a public OAuth client (the
//! authorization server advertises `token_endpoint_auth_methods_supported:
//! ["none"]`), so there is no client secret to protect. The only secret is the
//! OAuth token bundle, which lives in the OS keychain under `piekstra.rhap` (see
//! [`crate::credential`]).
//!
//! # The account is never global, defaulted, or ambient — by rule
//!
//! Authentication is global: one OAuth session in the keychain serves the whole
//! machine. **The account is not.** Any operation that acts against a specific
//! account requires the account to be named explicitly on that invocation
//! (`--account <ID>`, `required`) — there is **no** config key for it, **no**
//! env-var default, and **no** fallback of any kind.
//!
//! This is a deliberate safety rule, not an omission. An ambient/default account
//! is a wrong-account hazard under concurrent CLI use: two commands running at
//! once (or a stale exported env var, or a forgotten config default) could
//! silently act on the wrong account — read the wrong positions, or worse, place
//! or cancel an order there. Requiring the account per-operation makes every
//! account-scoped call self-describing and impossible to misdirect by ambient
//! state. It is also why the account number is never hardcoded here: it is
//! identifying, and it belongs on the command line, not in the source tree.

use serde::{Deserialize, Serialize};

/// Binary name: keychain service (`piekstra.rhap`), config dir, env prefix.
pub const BIN: &str = "rhap";
/// Repo, for `info`.
pub const REPO: &str = "piekstra/robinhood-agent-trading-cli";

/// The Robinhood Agentic Trading MCP endpoint (JSON-RPC 2.0 over Streamable
/// HTTP). This is the OAuth *protected resource* and the value passed as the
/// RFC 8707 `resource` indicator on the authorization and token requests.
pub const MCP_ENDPOINT: &str = "https://agent.robinhood.com/mcp/trading";

/// RFC 9728 protected-resource metadata URL (public, unauthenticated). The
/// origin's well-known path — discovery starts here and follows
/// `authorization_servers` to the RFC 8414 authorization-server metadata.
pub const PROTECTED_RESOURCE_METADATA: &str =
    "https://agent.robinhood.com/.well-known/oauth-protected-resource";

/// The scope the resource advertises (`scopes_supported: ["internal"]`).
pub const SCOPE: &str = "internal";

/// A human-readable client name sent at dynamic registration (RFC 7591).
pub const CLIENT_NAME: &str = "rhap (Robinhood Agentic Passthrough)";

/// Keychain account key for the OAuth token bundle (access + refresh + expiry).
pub const KEYCHAIN_TOKENS: &str = "tokens";

/// Config keys accepted by `config set`/`unset`.
///
/// Deliberately does **not** include `account`: the account is a required
/// per-operation flag, never a stored default (see the module docs).
pub const KNOWN_KEYS: &[&str] = &["client_id"];

/// On-disk, non-secret settings.
///
/// There is intentionally no `account` field — the account is never a stored
/// default (see the module docs). The only setting is the public OAuth client
/// id, which login writes and refresh reuses.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The public OAuth client id from dynamic client registration. Reused by
    /// `auth login` (authorize + token exchange) and token refresh. Not a
    /// secret — see the module docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_the_resource_indicator() {
        // The MCP endpoint and the OAuth resource indicator are the same URL —
        // the discovery document says so (resource == the mcp/trading URL).
        assert_eq!(MCP_ENDPOINT, "https://agent.robinhood.com/mcp/trading");
        assert!(PROTECTED_RESOURCE_METADATA.ends_with("/.well-known/oauth-protected-resource"));
    }

    #[test]
    fn account_is_not_a_config_key() {
        // The account must never be storable — an ambient default is the
        // wrong-account hazard this rule exists to prevent.
        assert!(!KNOWN_KEYS.contains(&"account"));
    }
}
