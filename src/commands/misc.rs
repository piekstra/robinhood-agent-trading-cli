//! `config` and `info` — the two commands that need neither a session nor the
//! network.

use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{output, CliError, CommonArgs};

use crate::config::{Config, BIN, KNOWN_KEYS, REPO};

/// A `config` operation, decoupled from the clap enum.
pub enum ConfigOp<'a> {
    Path,
    Show,
    Set(&'a str, &'a str),
    Unset(&'a str),
}

pub fn config(
    common: &CommonArgs,
    store: &ConfigStore,
    mut cfg: Config,
    op: ConfigOp<'_>,
) -> Result<(), CliError> {
    match op {
        ConfigOp::Path => {
            println!("{}", store.path()?.display());
            Ok(())
        }
        ConfigOp::Show => {
            let v = serde_json::to_value(&cfg).unwrap_or_default();
            if common.json {
                output::json(&v);
            } else {
                output::render(&v);
            }
            Ok(())
        }
        ConfigOp::Set(key, value) => {
            match key {
                "client_id" => cfg.client_id = Some(value.to_string()),
                // `account` is deliberately not a config key — it is required
                // per-operation, never a stored default (see config.rs).
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
        ConfigOp::Unset(key) => {
            match key {
                "client_id" => cfg.client_id = None,
                other => return Err(unknown_key(other)),
            }
            store.save(&cfg)
        }
    }
}

fn unknown_key(key: &str) -> CliError {
    CliError::Usage(format!(
        "unknown config key `{key}` (known: {})",
        KNOWN_KEYS.join(", ")
    ))
}

/// `info` — machine-readable capability discovery (cli-info/v1).
///
/// The DTO is inherently machine-shaped, so it is emitted as JSON in both
/// modes; `json_mode` only decides between pretty JSON and a rendered block.
pub fn info(json_mode: bool) -> Result<(), CliError> {
    let info = CliInfo::new(
        BIN,
        env!("CARGO_PKG_VERSION"),
        &format!("https://github.com/{REPO}"),
        AuthInfo {
            required: true,
            // The `auth status` AuthMethod enum has no OAuth variant, so the DTO
            // reports the nearest one; `info.auth.method` is the free-string
            // field a driver should trust. This credential is an OAuth 2.1 +
            // PKCE browser login against a dynamically-registered public client.
            method: "oauth2".into(),
            login_hint: Some(format!("{BIN} auth login")),
        },
        &[
            "tools",
            "accounts",
            "positions",
            "orders",
            "quotes",
            "order",
            "option",
            "portfolio",
            "pnl",
            "margin",
            "equity",
            "index",
            "market",
            "news",
            "sec",
            "alerts",
            "crypto",
            "search",
            "watchlists",
            "scans",
            "metrics",
            "ark",
            "call",
        ],
    );
    let v = serde_json::to_value(&info).unwrap();
    if json_mode {
        output::json(&v);
    } else {
        output::render(&v);
    }
    Ok(())
}
