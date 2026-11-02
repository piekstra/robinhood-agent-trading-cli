//! Command implementations, one module per group, plus the shared [`Ctx`] that
//! carries resolved settings and mints an authenticated MCP client.

pub mod args;
pub mod ark;
pub mod auth;
pub mod call;
pub mod data;
pub mod extras;
pub mod misc;
pub mod options;
pub mod order;
pub mod reads;
pub mod regime;
pub mod scans;
pub mod score;
pub mod tools;
pub mod watchlists;

use pk_cli_core::{CliError, CommonArgs};
use pk_cli_secrets::CredentialStore;

use crate::config::{Config, MCP_ENDPOINT};
use crate::credential::now_unix;
use crate::mcp::McpClient;
use crate::token_refresh::{current_tokens, refresh_lock_path, KeychainTokens};
use crate::{discovery, oauth};

/// Everything a networked command needs: the global flags, the loaded config,
/// and the keychain handle.
///
/// Deliberately carries **no** account: authentication is global, but the
/// account is never ambient. Account-scoped commands take a required
/// `--account` straight from their own clap args (see `config.rs`).
pub struct Ctx<'a> {
    pub common: &'a CommonArgs,
    pub cfg: &'a Config,
    pub creds: &'a CredentialStore,
}

impl<'a> Ctx<'a> {
    /// Build an authenticated MCP client, refreshing the access token first if
    /// it is at or past expiry.
    ///
    /// A refresh needs the token endpoint (discovered) and the registered
    /// client id (config). If either the token or the client id is missing, the
    /// error points at `rhap auth login`.
    pub fn client(&self) -> Result<McpClient, CliError> {
        let mut store = KeychainTokens(self.creds);
        let lock_path = refresh_lock_path()?;
        let tokens = current_tokens(&mut store, &lock_path, now_unix, |stale| {
            let client_id = self.cfg.client_id.as_deref().ok_or_else(|| {
                CliError::Auth(
                    "the access token is expired and no OAuth client id is stored to refresh it — \
                     run `rhap auth login`"
                        .into(),
                )
            })?;
            let http = pk_cli_http::client("rhap", env!("CARGO_PKG_VERSION"))?;
            let endpoints = discovery::discover(&http)?;
            let fresh = oauth::refresh_tokens(
                &http,
                &endpoints.token_endpoint,
                client_id,
                &stale.refresh_token,
                discovery::resource_indicator(),
                now_unix(),
            )?;
            if self.common.verbose {
                eprintln!("access token refreshed");
            }
            Ok(fresh)
        })?;

        McpClient::new(MCP_ENDPOINT, &tokens.access_token, self.common.verbose)
    }
}
