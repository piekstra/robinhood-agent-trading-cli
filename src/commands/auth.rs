//! `auth` — the OAuth 2.1 + PKCE login, session status, logout, and a raw
//! token-set write for rotation/headless setup.
//!
//! `login` performs a real browser round trip (see [`crate::oauth`]) and is the
//! owner's to run; the rest work offline. `status` never spends a request unless
//! `--verify` is passed.

use pk_cli_auth::{token, AuthMethod, AuthStatus, SetCredentialArgs};
use pk_cli_config::ConfigStore;
use pk_cli_core::{dates, CliError, CommonArgs};
use pk_cli_secrets::CredentialStore;

use crate::config::{Config, MCP_ENDPOINT, SCOPE};
use crate::credential::{self, now_unix, TokenSet};
use crate::mcp::McpClient;
use crate::{discovery, oauth};

/// The inputs `login` needs beyond the global flags.
pub struct LoginInput {
    /// Loopback port; `0` picks a free ephemeral port.
    pub port: u16,
    /// Replace an existing stored session instead of failing.
    pub overwrite: bool,
    /// Don't try to launch a browser (still prints the URL).
    pub no_open: bool,
    /// Skip the post-login `tools/list` verification request.
    pub no_verify: bool,
}

/// The full authorization-code-with-PKCE flow.
///
/// Owner-run only: it opens a browser and completes a real OAuth exchange. The
/// test suite never calls this; the pieces it is built from (PKCE, the
/// authorization URL, callback parsing, token parsing) are unit-tested in
/// [`crate::oauth`] and [`crate::credential`].
pub fn login(
    common: &CommonArgs,
    store: &ConfigStore,
    creds: &CredentialStore,
    mut cfg: Config,
    input: &LoginInput,
) -> Result<(), CliError> {
    if credential::read_tokens(creds)?.is_some() && !input.overwrite {
        return Err(CliError::Usage(
            "a session is already stored; pass --overwrite to replace it".into(),
        ));
    }

    let http = pk_cli_http::client("rhap", env!("CARGO_PKG_VERSION"))?;
    let endpoints = discovery::discover(&http)?;
    let registration_endpoint = endpoints.registration_endpoint.clone().ok_or_else(|| {
        CliError::Upstream(
            "the authorization server advertises no registration_endpoint — cannot register a \
             client (see docs/api.md)"
                .into(),
        )
    })?;

    // Bind the loopback listener first so the redirect_uri (which the client
    // must be registered with) carries the real port.
    let listener = oauth::LoopbackListener::bind(input.port)?;
    let redirect_uri = listener.redirect_uri();

    // Dynamic client registration (RFC 7591): a fresh public client bound to
    // this redirect. The issued id is public and cached for later refreshes.
    let client_id = oauth::register_client(&http, &registration_endpoint, &redirect_uri)?;
    cfg.client_id = Some(client_id.clone());
    store.save(&cfg)?;

    let pkce = oauth::Pkce::generate();
    let state = oauth::random_urlsafe(24);
    let url = oauth::authorize_url(
        &endpoints.authorization_endpoint,
        &client_id,
        &redirect_uri,
        &pkce.challenge,
        &state,
        SCOPE,
        discovery::resource_indicator(),
    );

    if !common.quiet {
        eprintln!("Open this URL to authorize rhap with Robinhood, then approve in the browser:\n");
    }
    // The URL is data; print it on stdout so it can be copied even under -q.
    println!("{url}");
    if !input.no_open && oauth::open_browser(&url) && !common.quiet {
        eprintln!("\n(opened in your default browser; waiting for the callback…)");
    } else if !common.quiet {
        eprintln!("\nwaiting for the callback on {redirect_uri} …");
    }

    let code = listener.wait_for_code(&state)?;
    let tokens = oauth::exchange_code(
        &http,
        &endpoints.token_endpoint,
        &client_id,
        &code,
        &redirect_uri,
        &pkce.verifier,
        discovery::resource_indicator(),
        now_unix(),
    )?;
    credential::write_tokens(creds, &tokens)?;

    if !input.no_verify {
        // The one cheap read that proves the whole chain works.
        let mut client = McpClient::new(MCP_ENDPOINT, &tokens.access_token, common.verbose)?;
        client.tools_list()?;
        if !common.quiet {
            eprintln!("verified: the MCP server accepted the token and listed its tools");
        }
    }
    if !common.quiet {
        eprintln!("logged in. Access token stored in the OS keychain (piekstra.rhap).");
    }
    Ok(())
}

/// `auth status` — offline unless `--verify`.
///
/// Reports the *global* session only. It names no account: the account is never
/// ambient (see `config.rs`), so there is nothing account-scoped to report here.
pub fn status(common: &CommonArgs, creds: &CredentialStore, verify: bool) -> Result<(), CliError> {
    let (raw, note) = credential::peek(creds);
    let tokens = raw.as_ref().and_then(|s| TokenSet::parse(s.expose()).ok());
    let now = now_unix();

    // "Authenticated" = usable without a browser round trip. A present token is
    // usable even with an expired access token, because a refresh token renews
    // it silently.
    let mut authenticated = tokens.is_some();
    let mut session_valid = tokens.as_ref().map(|t| !t.access_expired(now));

    // Prefer the JWT `exp` when the access token is a JWT; otherwise fall back
    // to the stored absolute expiry.
    let expires_at = tokens.as_ref().and_then(|t| {
        token::expires_at(&t.access_token).or_else(|| Some(dates::fmt_rfc3339(t.expires_at as i64)))
    });

    if verify {
        if let Some(t) = &tokens {
            match McpClient::new(MCP_ENDPOINT, &t.access_token, common.verbose)
                .and_then(|mut c| c.tools_list())
            {
                Ok(_) => session_valid = Some(true),
                Err(CliError::Auth(_)) => {
                    session_valid = Some(false);
                    authenticated = false;
                }
                Err(e) => return Err(e),
            }
        }
    }

    let mut st = AuthStatus::new(true, authenticated, AuthMethod::Password);
    // No account: the session is global, the account is never ambient.
    st.credential_in_keychain = Some(tokens.is_some());
    st.session_valid = session_valid;
    st.expires_at = expires_at;
    st.emit(common.json);

    if !common.quiet {
        if let Some(n) = note {
            eprintln!("{n}");
        }
        match &tokens {
            None => eprintln!("no session stored — run `rhap auth login`"),
            Some(t) if t.access_expired(now) => {
                eprintln!("access token expired; it refreshes automatically on the next call")
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// `auth logout` — drop the session; `--forget` also clears the stored config
/// (the OAuth client id).
pub fn logout(
    common: &CommonArgs,
    store: &ConfigStore,
    creds: &CredentialStore,
    forget: bool,
) -> Result<(), CliError> {
    creds.delete(crate::config::KEYCHAIN_TOKENS)?;
    if forget {
        store.clear()?;
        if !common.quiet {
            eprintln!("session cleared; stored config (client id) removed");
        }
    } else if !common.quiet {
        eprintln!("session cleared. The OAuth client id stays in config for the next login.");
    }
    Ok(())
}

/// `auth set-credential` — raw keychain write of a token bundle (rotation /
/// headless). Expects a [`TokenSet`] JSON via `--stdin`/`--from-env`.
pub fn set_credential(
    common: &CommonArgs,
    creds: &CredentialStore,
    args: &SetCredentialArgs,
) -> Result<(), CliError> {
    if credential::read_tokens(creds)?.is_some() && !args.overwrite {
        return Err(CliError::Usage(
            "a session is already stored; pass --overwrite to replace it".into(),
        ));
    }
    let secret = args.source.read(None)?;
    // Parse before storing so a malformed bundle fails at write time, not on
    // first use.
    let tokens = TokenSet::parse(secret.expose())?;
    credential::write_tokens(creds, &tokens)?;
    if !common.quiet {
        eprintln!("token bundle stored in the OS keychain (piekstra.rhap)");
    }
    Ok(())
}
