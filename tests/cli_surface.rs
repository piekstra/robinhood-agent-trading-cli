//! Black-box tests over the built `rhap` binary. Fully offline: nothing here
//! reaches the network or the live MCP, and every test that could read the
//! keychain is either config-isolated or opt-in.

use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use predicates::str::contains;

/// Tests that reach the OS keychain are opt-in.
///
/// On macOS `cargo test` rebuilds an ad-hoc-signed binary each run, which the OS
/// treats as a new code identity — so a keychain read can raise a permission
/// dialog. On Linux CI there is no D-Bus session, so the keyring backend errors
/// (exit 1) instead of reporting "no entry" (exit 3). Neither is a CLI bug and
/// neither belongs in an offline gate. Run them with
/// `RHAP_TEST_KEYCHAIN=1 cargo test`.
fn keychain_tests_enabled() -> bool {
    std::env::var("RHAP_TEST_KEYCHAIN").as_deref() == Ok("1")
}

fn rhap() -> Command {
    Command::cargo_bin("rhap").expect("binary builds")
}

/// A throwaway config path so a test never reads the developer's real config.
fn scratch_config(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhap-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("config.json")
}

/// Run with an isolated config and no ambient account selection.
fn isolated(tag: &str) -> Command {
    let mut c = rhap();
    c.arg("--config").arg(scratch_config(tag));
    c.env_remove("RHAP_ACCOUNT");
    c
}

const GROUPS: &[&str] = &[
    "auth",
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
    "config",
    "completions",
    "info",
];

#[test]
fn top_level_help_lists_the_surface() {
    let out = rhap().arg("--help").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    for g in GROUPS {
        assert!(text.contains(g), "top-level help is missing `{g}`:\n{text}");
    }
    // The global flags. There is deliberately NO global `--account`: it is a
    // required per-subcommand flag (the account is never ambient).
    for flag in ["--json", "--verbose", "--quiet", "--no-color"] {
        assert!(text.contains(flag), "top-level help is missing `{flag}`");
    }
    assert!(
        !text.contains("--account"),
        "`--account` must NOT be a global flag — it is per-operation and required"
    );
}

/// Rendering each subcommand's help forces clap's debug assertions over that
/// subtree.
#[test]
fn every_subcommand_help_renders() {
    for g in GROUPS {
        rhap().arg(g).arg("--help").assert().success();
    }
}

#[test]
fn nested_subcommand_help_renders() {
    let nested = [
        ["auth", "login"],
        ["auth", "status"],
        ["auth", "logout"],
        ["auth", "set-credential"],
        ["tools", "list"],
        ["accounts", "list"],
        ["positions", "list"],
        ["orders", "list"],
        ["quotes", "get"],
        ["order", "place"],
        ["order", "cancel"],
        ["option", "positions"],
        ["option", "orders"],
        ["option", "quotes"],
        ["option", "chains"],
        ["option", "order"],
        ["config", "set"],
    ];
    for pair in nested {
        rhap()
            .args(pair)
            .arg("--help")
            .assert()
            .success()
            .stdout(contains("Usage"));
    }
}

#[test]
fn version_prints_the_crate_version() {
    rhap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn info_emits_cli_info_dto() {
    let out = rhap().args(["info", "--json"]).assert().success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("info emits JSON");
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["spec"], "piekstra-cli/1");
    assert_eq!(v["name"], "rhap");
    assert_eq!(v["auth"]["required"], true);
    assert_eq!(v["auth"]["method"], "oauth2");
    let caps = v["capabilities"].as_array().expect("capabilities array");
    for expected in [
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
        "search",
        "watchlists",
        "scans",
        "call",
    ] {
        assert!(
            caps.iter().any(|c| c == expected),
            "capabilities missing {expected}"
        );
    }
}

#[test]
fn unknown_command_is_a_usage_error() {
    rhap().arg("nope").assert().code(2);
}

/// Dates are validated before any config load, so this is a usage error even on
/// a machine that has never been configured.
#[test]
fn bad_date_is_a_usage_error() {
    isolated("baddate")
        .args([
            "orders",
            "list",
            "--account",
            "SAMPLE01",
            "--since",
            "06/30/2026",
        ])
        .assert()
        .code(2);
}

/// `get_equity_orders` has only a `created_at_gte` filter, so `orders list`
/// exposes only `--since`. `--until` is not a silent no-op — it is not a flag at
/// all, and clap rejects it (exit 2).
#[test]
fn orders_list_has_no_until_flag() {
    isolated("nountil")
        .args([
            "orders",
            "list",
            "--account",
            "SAMPLE01",
            "--until",
            "2026-01-01",
        ])
        .assert()
        .code(2)
        .stderr(contains("unexpected argument"));
}

/// The write path validates its order terms before touching anything, so a bad
/// side/tif/quantity is a usage error regardless of session state.
#[test]
fn bad_order_terms_are_usage_errors() {
    isolated("badside")
        .args([
            "order",
            "place",
            "--symbol",
            "SAMPL",
            "--side",
            "hold",
            "--qty",
            "1",
            "--limit",
            "1",
            "--tif",
            "day",
            "--account",
            "SMOKE",
        ])
        .assert()
        .code(2)
        .stderr(contains("--side"));
    isolated("badtif")
        .args([
            "order",
            "place",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "1",
            "--limit",
            "1",
            "--tif",
            "week",
            "--account",
            "SMOKE",
        ])
        .assert()
        .code(2)
        .stderr(contains("--tif"));
    isolated("badqty")
        .args([
            "order",
            "place",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "0",
            "--limit",
            "1",
            "--tif",
            "day",
            "--account",
            "SMOKE",
        ])
        .assert()
        .code(2);
}

/// The whole point of `--dry-run`: build and print the exact JSON-RPC request,
/// send nothing, touch no keychain. This runs with no session configured.
#[test]
fn order_place_dry_run_prints_the_request_without_sending() {
    let out = isolated("dryplace")
        .args([
            "order",
            "place",
            "--symbol",
            "sampl",
            "--side",
            "buy",
            "--qty",
            "2",
            "--limit",
            "10.00",
            "--tif",
            "day",
            "--account",
            "SAMPLE01",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "order-dry-run/v1");
    assert_eq!(v["would_send"], false);
    assert_eq!(v["request"]["method"], "tools/call");
    assert_eq!(v["request"]["params"]["name"], "place_equity_order");
    let args = &v["request"]["params"]["arguments"];
    assert_eq!(args["symbol"], "SAMPL"); // upper-cased
    assert_eq!(args["side"], "buy");
    // VERIFIED 2026-08-27: the field is `type`, and there is no `extended_hours`.
    assert_eq!(args["type"], "limit");
    assert!(args.get("order_type").is_none(), "the live field is `type`");
    assert!(args.get("extended_hours").is_none());
    assert_eq!(args["time_in_force"], "gfd"); // `day` -> gfd
    assert!(args["quantity"].is_string(), "quantity stays a string");
}

/// Shared argv for the order-place refusal tests.
fn place_argv<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec![
        "order",
        "place",
        "--symbol",
        "SAMPL",
        "--account",
        "SAMPLE01",
        "--tif",
        "day",
        "--dry-run",
        "--json",
    ];
    v.extend_from_slice(extra);
    v
}

/// The order-placement guards added 2026-09-18 were covered only by a shell
/// smoke target that CI never runs, written as `! rhap … >/dev/null 2>&1` —
/// which passes on ANY failure, including a clap parse error. Deleting the
/// entire `validate()` arm for `order place` left CI green. These assert the
/// exit code AND the message, so a refusal for the wrong reason fails.
#[test]
fn a_fractional_quantity_on_a_limit_order_is_refused_with_the_fix_named() {
    let out = isolated("fraclimit")
        .args(place_argv(&[
            "--side", "sell", "--qty", "0.968395", "--limit", "760.00",
        ]))
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(err.contains("fractional"), "{err}");
    assert!(
        err.contains("--type market"),
        "the refusal must name the fix: {err}"
    );
}

#[test]
fn a_fractional_market_order_builds_with_no_limit_price() {
    let out = isolated("fracmkt")
        .args(place_argv(&[
            "--side", "sell", "--qty", "0.5", "--type", "market",
        ]))
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let args = &v["request"]["params"]["arguments"];
    assert_eq!(args["type"], "market");
    assert_eq!(args["quantity"], "0.5");
    assert!(args.get("limit_price").is_none(), "absent, not null");
    assert_eq!(args["market_hours"], "regular_hours");
}

#[test]
fn both_sizing_fields_together_are_refused() {
    let out = isolated("bothsize")
        .args(place_argv(&[
            "--side",
            "buy",
            "--qty",
            "1",
            "--dollar-amount",
            "100",
            "--type",
            "market",
        ]))
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        err.contains("exactly one of --qty or --dollar-amount"),
        "{err}"
    );
}

#[test]
fn a_dollar_amount_on_a_limit_order_is_refused() {
    let out = isolated("dollarlimit")
        .args(place_argv(&[
            "--side",
            "buy",
            "--dollar-amount",
            "100",
            "--limit",
            "10",
        ]))
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        err.contains("--dollar-amount requires --type market"),
        "{err}"
    );
}

#[test]
fn a_limit_price_on_a_market_order_is_refused() {
    let out = isolated("mktlimit")
        .args(place_argv(&[
            "--side", "buy", "--qty", "1", "--type", "market", "--limit", "10",
        ]))
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        err.contains("--limit is not valid with --type market"),
        "{err}"
    );
}

#[test]
fn a_limit_order_with_no_limit_price_is_refused() {
    let out = isolated("nolimit")
        .args(place_argv(&["--side", "buy", "--qty", "1"]))
        .assert()
        .failure()
        .code(2);
    let err = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(err.contains("--limit is required"), "{err}");
}

#[test]
fn order_cancel_dry_run_prints_the_request_without_sending() {
    let out = isolated("drycancel")
        .args([
            "order",
            "cancel",
            "SAMPLE-ORDER-ID",
            "--account",
            "SAMPLE01",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "order-dry-run/v1");
    assert_eq!(v["request"]["params"]["name"], "cancel_equity_order");
    let args = &v["request"]["params"]["arguments"];
    assert_eq!(args["order_id"], "SAMPLE-ORDER-ID");
    assert_eq!(args["account_number"], "SAMPLE01"); // the required scope
}

/// A LIVE place with no way to confirm (non-interactive `--json`, no `--force`)
/// must exit 6 — before any keychain read or network call.
#[test]
fn a_live_order_without_confirmation_exits_six() {
    let out = isolated("placeconfirm")
        .args([
            "order",
            "place",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "1",
            "--limit",
            "1.00",
            "--tif",
            "day",
            "--account",
            "SAMPLE01",
            "--json",
        ])
        .assert()
        .code(6);
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("error DTO on stdout");
    assert_eq!(v["error"]["code"], "confirmation_required");
}

#[test]
fn a_live_cancel_without_confirmation_exits_six() {
    isolated("cancelconfirm")
        .args([
            "order",
            "cancel",
            "SAMPLE-ORDER-ID",
            "--account",
            "SAMPLE01",
            "--json",
        ])
        .assert()
        .code(6);
}

#[test]
fn json_errors_use_the_family_error_dto() {
    // --account is supplied so the failure is our date validator (a family
    // error DTO), not clap's missing-required-arg error.
    let out = isolated("jsonerr")
        .args([
            "orders",
            "list",
            "--account",
            "SAMPLE01",
            "--since",
            "nope",
            "--json",
        ])
        .assert()
        .code(2);
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("error DTO on stdout");
    assert_eq!(v["error"]["code"], "usage");
    assert!(v["error"]["message"].is_string());
}

/// `config` round-trips the client_id key without any keychain or network.
#[test]
fn config_roundtrips_the_client_id_key() {
    let cfg = scratch_config("configset");
    let _ = std::fs::remove_file(&cfg);

    rhap()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "set", "client_id", "sample-client-id-000000"])
        .assert()
        .success();

    let out = rhap()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "show", "--json"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("config show emits JSON");
    assert_eq!(v["client_id"], "sample-client-id-000000");
}

#[test]
fn config_rejects_an_unknown_key() {
    isolated("configbad")
        .args(["config", "set", "password", "hunter2"])
        .assert()
        .code(2);
}

/// `account` must NOT be a config key — an ambient/stored default account is the
/// wrong-account hazard the design forbids.
#[test]
fn config_refuses_an_account_key() {
    isolated("configacct")
        .args(["config", "set", "account", "SAMPLE01"])
        .assert()
        .code(2);
}

/// Every account-scoped command requires `--account` explicitly; omitting it is
/// a usage error (exit 2), decided by clap before any config/keychain/network.
#[test]
fn account_scoped_commands_require_the_account_flag() {
    let cases: &[&[&str]] = &[
        &["positions", "list"],
        &["orders", "list"],
        &[
            "order",
            "place",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "1",
            "--limit",
            "1",
            "--tif",
            "day",
            "--dry-run",
        ],
        &["order", "cancel", "SAMPLE-ORDER-ID", "--dry-run"],
        // Option account-scoped commands carry the same required --account.
        &["option", "positions", "list"],
        &["option", "orders", "list"],
        &[
            "option",
            "order",
            "place",
            "--quantity",
            "1",
            "--instrument-id",
            "INST-1",
            "--position-effect",
            "open",
            "--side",
            "buy",
            "--dry-run",
        ],
        &["option", "order", "cancel", "SAMPLE-ORDER-ID", "--dry-run"],
        &[
            "option",
            "exercise",
            "--option-id",
            "INST-1",
            "--quantity",
            "1",
            "--dry-run",
        ],
        &["option", "exercise-cancel", "INST-1", "--dry-run"],
        // Other account-scoped reads/writes across the new groups.
        &["portfolio", "get"],
        &["pnl", "trade-history"],
        &["pnl", "realized"],
        &["margin", "equity-upgrade-info"],
        &["margin", "option-upgrade-info"],
        &["equity", "tax-lots", "--symbol", "AAPL"],
        &["equity", "tradability", "--symbols", "AAPL"],
    ];
    for (i, args) in cases.iter().enumerate() {
        isolated(&format!("needacct{i}"))
            .args(*args)
            .assert()
            .code(2);
    }
}

/// The account is never read from an env var. Even with `RHAP_ACCOUNT` set,
/// omitting `--account` is still a usage error — proving there is no env
/// fallback (the wrong-account hazard under a stale exported var).
#[test]
fn no_env_account_fallback() {
    rhap()
        .arg("--config")
        .arg(scratch_config("noenvfallback"))
        .env("RHAP_ACCOUNT", "SAMPLE01")
        .args(["positions", "list"])
        .assert()
        .code(2);
}

/// Nothing on the account-scoped path consults a stored config account either:
/// a config file that (somehow) carries an `account` field is ignored, and the
/// command still demands `--account`.
#[test]
fn no_config_account_fallback() {
    let cfg = scratch_config("noconfigfallback");
    // Write a config that includes a stray `account` — the loader ignores
    // unknown fields, and no code path reads one.
    std::fs::write(
        &cfg,
        r#"{"account":"SAMPLE01","client_id":"sample-client-id-000000"}"#,
    )
    .expect("write config");
    rhap()
        .arg("--config")
        .arg(&cfg)
        .env_remove("RHAP_ACCOUNT")
        .args(["positions", "list"])
        .assert()
        .code(2);
}

// ------------------------------------------------------------- option surface

/// `option order place --dry-run` (single-leg) builds a `place_option_order`
/// tools/call with the required account_number, a one-element legs array, and a
/// string quantity — and sends nothing.
#[test]
fn option_order_place_dry_run_single_leg() {
    let out = isolated("optplace")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "2",
            "--instrument-id",
            "INST-1",
            "--position-effect",
            "open",
            "--side",
            "buy",
            "--type",
            "limit",
            "--price",
            "1.25",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "option-order-dry-run/v1");
    assert_eq!(v["request"]["params"]["name"], "place_option_order");
    let args = &v["request"]["params"]["arguments"];
    assert_eq!(args["account_number"], "SAMPLE01"); // required scope
    assert!(args["quantity"].is_string());
    let legs = args["legs"].as_array().expect("legs array");
    assert_eq!(legs.len(), 1);
    assert_eq!(
        legs[0]["option_id"], "INST-1",
        "the schema names the leg by option_id"
    );
    assert_eq!(legs[0]["position_effect"], "open");
    assert_eq!(legs[0]["side"], "buy");
    assert_eq!(
        legs[0]["ratio_quantity"], 1,
        "ratio_quantity is an integer on the wire"
    );
    assert_eq!(args["type"], "limit");
    assert_eq!(args["price"], "1.25");
}

/// The multi-leg passthrough: `--legs <JSON>` is forwarded verbatim.
#[test]
fn option_order_place_dry_run_legs_passthrough() {
    let legs = r#"[{"instrument_id":"A","side":"buy","position_effect":"open","ratio_quantity":"1"},{"instrument_id":"B","side":"sell","position_effect":"open","ratio_quantity":"1"}]"#;
    let out = isolated("optlegs")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "1",
            "--legs",
            legs,
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(
        v["request"]["params"]["arguments"]["legs"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

#[test]
fn option_order_cancel_dry_run() {
    let out = isolated("optcancel")
        .args([
            "option",
            "order",
            "cancel",
            "SAMPLE-ORDER-ID",
            "--account",
            "SAMPLE01",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "option-order-dry-run/v1");
    assert_eq!(v["request"]["params"]["name"], "cancel_option_order");
    let args = &v["request"]["params"]["arguments"];
    assert_eq!(args["account_number"], "SAMPLE01");
    assert_eq!(args["order_id"], "SAMPLE-ORDER-ID");
}

/// A LIVE option place with no way to confirm exits 6 — before any keychain read.
#[test]
fn a_live_option_order_without_confirmation_exits_six() {
    isolated("optconfirm")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "1",
            "--instrument-id",
            "INST-1",
            "--position-effect",
            "open",
            "--side",
            "buy",
            "--json",
        ])
        .assert()
        .code(6);
}

#[test]
fn option_reads_require_their_arguments() {
    // Quotes need instrument ids; chains need an underlying.
    isolated("optq")
        .args(["option", "quotes", "get"])
        .assert()
        .code(2);
    isolated("optc")
        .args(["option", "chains", "get"])
        .assert()
        .code(2);
}

/// The option write path validates its terms before touching anything.
#[test]
fn option_place_bad_terms_are_usage_errors() {
    // Bad direction.
    isolated("optbaddir")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "1",
            "--instrument-id",
            "INST-1",
            "--position-effect",
            "open",
            "--side",
            "buy",
            "--direction",
            "sideways",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stderr(contains("--direction"));
    // Bad position effect.
    isolated("optbadeff")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "1",
            "--instrument-id",
            "INST-1",
            "--position-effect",
            "roll",
            "--side",
            "buy",
            "--dry-run",
        ])
        .assert()
        .code(2);
    // No legs at all.
    isolated("optnolegs")
        .args([
            "option",
            "order",
            "place",
            "--account",
            "SAMPLE01",
            "--quantity",
            "1",
            "--dry-run",
        ])
        .assert()
        .code(2);
}

// ------------------------------------------------------- generic `call` layer

/// An unknown tool name is a not-found error (exit 4).
#[test]
fn call_rejects_an_unknown_tool() {
    isolated("callunknown")
        .args(["call", "delete_everything", "--dry-run"])
        .assert()
        .code(4);
}

/// A read tool passes through freely — `--dry-run` builds the request offline.
#[test]
fn call_read_dry_run_builds_the_request() {
    let out = isolated("callread")
        .args(["call", "get_accounts", "--dry-run", "--json"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "call-dry-run/v1");
    assert_eq!(v["mutating"], false);
    assert_eq!(v["request"]["params"]["name"], "get_accounts");
}

/// A mutating account-scoped tool with no `account_number` in `--args` is a
/// usage error (exit 2) — the no-ambient-account rule on the generic path.
#[test]
fn call_refuses_a_mutating_account_scoped_tool_without_account() {
    isolated("callnoacct")
        .args(["call", "place_equity_order", "--json"])
        .assert()
        .code(2)
        .stderr(contains("account_number"));
    // Even with --force, the missing account is still refused.
    isolated("callnoacctforce")
        .args(["call", "cancel_option_exercise", "--force", "--json"])
        .assert()
        .code(2);
}

/// A mutating tool with the account present but neither `--dry-run` nor
/// `--force` refuses to send (exit 6).
#[test]
fn call_refuses_a_live_mutation_without_force() {
    // Account-scoped, account supplied, but no --force/--dry-run.
    isolated("callmutacct")
        .args([
            "call",
            "place_equity_order",
            "--args",
            r#"{"account_number":"SAMPLE01"}"#,
            "--json",
        ])
        .assert()
        .code(6);
    // Non-account-scoped mutating tool: same rule.
    isolated("callmutwatch")
        .args([
            "call",
            "create_watchlist",
            "--args",
            r#"{"display_name":"x"}"#,
            "--json",
        ])
        .assert()
        .code(6);
}

/// A mutating tool with `--dry-run` builds and prints the request without
/// sending — offline, and flagged mutating.
#[test]
fn call_mutating_dry_run_builds_the_request() {
    let out = isolated("callmutdry")
        .args([
            "call",
            "place_equity_order",
            "--args",
            r#"{"account_number":"SAMPLE01","symbol":"AAPL","side":"buy","type":"limit"}"#,
            "--dry-run",
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("dry-run emits JSON");
    assert_eq!(v["schema"], "call-dry-run/v1");
    assert_eq!(v["mutating"], true);
    assert_eq!(v["request"]["params"]["name"], "place_equity_order");
    assert_eq!(
        v["request"]["params"]["arguments"]["account_number"],
        "SAMPLE01"
    );
}

/// `call --args` must be a JSON object.
#[test]
fn call_rejects_non_object_args() {
    isolated("callbadargs")
        .args(["call", "get_accounts", "--args", "[1,2]", "--dry-run"])
        .assert()
        .code(2);
}

// --------------------------------------------------------- `tools show` (offline)

/// `tools show <NAME>` serves the schema from the embedded catalog — offline,
/// no token — and reports the mutating/account-scoped classification.
#[test]
fn tools_show_serves_the_embedded_schema_offline() {
    let out = isolated("toolsshow")
        .args(["tools", "show", "get_portfolio", "--json"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("tools show emits JSON");
    assert_eq!(v["schema"], "tool-schema/v1");
    assert_eq!(v["name"], "get_portfolio");
    assert_eq!(v["mutating"], false);
    assert_eq!(v["account_scoped"], true);
    assert!(v["input_schema"]["properties"]["account_number"].is_object());

    // A mutating tool is flagged as such.
    let out = isolated("toolsshowmut")
        .args(["tools", "show", "place_option_order", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["mutating"], true);
    assert_eq!(v["account_scoped"], true);
}

#[test]
fn tools_show_unknown_is_not_found() {
    isolated("toolsshowbad")
        .args(["tools", "show", "nope"])
        .assert()
        .code(4);
}

/// A sampling of the new typed subcommands render their help (clap debug
/// assertions run over each subtree).
#[test]
fn new_subcommand_help_renders() {
    let nested = [
        ["portfolio", "get"],
        ["pnl", "realized"],
        ["margin", "equity-upgrade-info"],
        ["equity", "fundamentals"],
        ["equity", "technicals"],
        ["index", "quotes"],
        ["market", "earnings-results"],
        ["watchlists", "create"],
        ["watchlists", "add-option"],
        ["scans", "update-filters"],
        ["option", "instruments"],
        ["option", "exercise"],
        ["tools", "show"],
    ];
    for pair in nested {
        rhap()
            .args(pair)
            .arg("--help")
            .assert()
            .success()
            .stdout(contains("Usage"));
    }
}

#[test]
fn completions_generate_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        rhap()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicates::str::is_empty().not());
    }
}

/// Any non-empty `NO_COLOR` disables color (no-color.org); none of the
/// conventional values may turn into a usage error.
#[test]
fn no_color_in_the_environment_never_fails_a_command() {
    for v in ["1", "true", "0", ""] {
        rhap()
            .env("NO_COLOR", v)
            .args(["completions", "bash"])
            .assert()
            .success();
    }
}

/// With no session stored, a read must fail as auth (3) pointing at login — not
/// panic, not report empty data. Opt-in because it reads the keychain.
#[test]
fn a_read_without_a_session_is_an_auth_error() {
    if !keychain_tests_enabled() {
        return;
    }
    isolated("noauth")
        .args(["accounts", "list"])
        .assert()
        .code(3)
        .stderr(contains("auth login"));
}

// ------------------------------------------------------------------ metrics

/// `metrics list` is offline: no config, keychain, or network — and it names
/// the published codenames with their weights.
#[test]
fn metrics_list_is_offline_and_names_harvest_1() {
    isolated("metrics")
        .args(["metrics", "list", "--json"])
        .assert()
        .success()
        .stdout(contains("\"schema\""))
        .stdout(contains("harvest/1"))
        .stdout(contains("credit_over_width"));
}

/// `option score` validates its terms before any keychain read: an unknown
/// metric, a bad type, a bad date, or an inverted delta band is a usage error.
#[test]
fn option_score_terms_are_validated_up_front() {
    let base = [
        "option",
        "score",
        "--underlying",
        "SAMPL",
        "--expiration",
        "2026-10-16",
    ];
    isolated("score-metric")
        .args(base)
        .args(["--metric", "nope/9"])
        .assert()
        .code(2)
        .stderr(contains("unknown metric"));
    isolated("score-type")
        .args(base)
        .args(["--type", "straddle"])
        .assert()
        .code(2);
    isolated("score-date")
        .args([
            "option",
            "score",
            "--underlying",
            "SAMPL",
            "--expiration",
            "10/16/2026",
        ])
        .assert()
        .code(2);
    isolated("score-delta")
        .args(base)
        .args(["--min-delta", "0.5", "--max-delta", "0.2"])
        .assert()
        .code(2);
    isolated("score-class")
        .args(base)
        .args(["--class", "meme"])
        .assert()
        .code(2);
}

/// `market regime` validates the metric kind before any keychain read.
#[test]
fn market_regime_rejects_a_score_metric_and_unknown_names() {
    isolated("regime-kind")
        .args(["market", "regime", "--metric", "harvest/1"])
        .assert()
        .code(2)
        .stderr(contains("not a state metric"));
    isolated("regime-unknown")
        .args(["market", "regime", "--metric", "mode/7"])
        .assert()
        .code(2);
}

/// `ark score` validates its metric kind and window before any network.
#[test]
fn ark_score_terms_are_validated_up_front() {
    isolated("ark-kind")
        .args(["ark", "score", "--metric", "harvest/1"])
        .assert()
        .code(2)
        .stderr(contains("not a ticker-score metric"));
    isolated("ark-window")
        .args(["ark", "score", "--window", "2"])
        .assert()
        .code(2);
}

// ------------------------------------------- 2026-09-17 surface additions

/// OCO terms are validated before any keychain read: fractional shares,
/// identical legs, and a bad side are usage errors.
#[test]
fn oco_terms_are_validated_up_front() {
    let base = [
        "order",
        "advanced",
        "place",
        "--account",
        "SMOKE",
        "--symbol",
        "SAMPL",
        "--side",
        "sell",
        "--quantity",
        "10",
        "--take-profit",
        "12.00",
        "--stop-loss",
        "8.00",
    ];
    isolated("oco-ok")
        .args(base)
        .arg("--dry-run")
        .arg("--json")
        .assert()
        .success();
    let mut frac = isolated("oco-frac");
    frac.args([
        "order",
        "advanced",
        "place",
        "--account",
        "SMOKE",
        "--symbol",
        "SAMPL",
        "--side",
        "sell",
        "--quantity",
        "1.5",
        "--take-profit",
        "12.00",
        "--stop-loss",
        "8.00",
        "--dry-run",
    ]);
    frac.assert().code(2).stderr(contains("whole shares"));
    let mut same = isolated("oco-same");
    same.args([
        "order",
        "advanced",
        "place",
        "--account",
        "SMOKE",
        "--symbol",
        "SAMPL",
        "--side",
        "sell",
        "--quantity",
        "10",
        "--take-profit",
        "9.00",
        "--stop-loss",
        "9.00",
        "--dry-run",
    ]);
    same.assert().code(2).stderr(contains("must differ"));
}

/// A crypto order needs exactly one of quantity / dollar-amount, and the
/// prices its type requires — all checked before the keychain.
#[test]
fn crypto_order_terms_are_validated_up_front() {
    let mut both = isolated("crypto-both");
    both.args([
        "crypto",
        "order",
        "place",
        "--account",
        "12345",
        "--symbol",
        "BTC",
        "--side",
        "buy",
        "--type",
        "market",
        "--quantity",
        "0.001",
        "--dollar-amount",
        "100",
        "--dry-run",
    ]);
    both.assert().code(2).stderr(contains("exactly one"));
    let mut limit = isolated("crypto-limit");
    limit.args([
        "crypto",
        "order",
        "place",
        "--account",
        "12345",
        "--symbol",
        "BTC",
        "--side",
        "buy",
        "--type",
        "limit",
        "--quantity",
        "0.001",
        "--dry-run",
    ]);
    limit.assert().code(2).stderr(contains("--limit-price"));
    let mut ok = isolated("crypto-ok");
    ok.args([
        "crypto",
        "order",
        "place",
        "--account",
        "12345",
        "--symbol",
        "BTC",
        "--side",
        "buy",
        "--type",
        "market",
        "--dollar-amount",
        "100",
        "--dry-run",
        "--json",
    ]);
    ok.assert().success().stdout(contains("place_crypto_order"));
}

/// `alerts mark-read` needs something to mark.
#[test]
fn mark_read_needs_ids_or_a_cutoff() {
    isolated("alerts-markread")
        .args(["alerts", "mark-read", "--dry-run"])
        .assert()
        .code(2)
        .stderr(contains("--ids"));
}

/// Audit 2026-09-23 security #2: alert deletes and mark-read were classified
/// as reads and ran with no confirmation. Both are writes now.
#[test]
fn alert_writes_need_confirmation() {
    isolated("alerts-delete-live")
        .args(["alerts", "delete", "SAMPLE-ALERT", "--json"])
        .assert()
        .code(6);
    isolated("alerts-markread-live")
        .args(["alerts", "mark-read", "--ids", "A1", "--json"])
        .assert()
        .code(6);
    for tool in ["delete_alert", "mark_alerts_read"] {
        isolated(&format!("call-{tool}"))
            .args(["call", tool, "--args", "{}", "--json"])
            .assert()
            .code(6);
    }
}

/// `run_scan` is annotated read-only: `tools show` reports it as a read.
#[test]
fn run_scan_is_a_read_and_delete_alert_a_write() {
    let out = rhap()
        .args(["tools", "show", "run_scan", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(
        v["data"]["mutating"].as_bool().or(v["mutating"].as_bool()),
        Some(false)
    );
    let out = rhap()
        .args(["tools", "show", "delete_alert", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(
        v["data"]["mutating"].as_bool().or(v["mutating"].as_bool()),
        Some(true)
    );
}

/// Audit 2026-09-23 security #6, through the binary: each value is refused
/// with exit 2 before config, keychain or network.
#[test]
fn order_numbers_accept_only_plain_decimals() {
    for (tag, qty, limit) in [
        ("inf", "inf", "1.00"),
        ("infinity", "Infinity", "1.00"),
        ("exp", "1e3", "1.00"),
        ("huge", "1e999", "1.00"),
        ("nan", "nan", "1.00"),
        ("halfexp", "5e-1", "1.00"),
        ("limitinf", "1", "inf"),
        ("limitexp", "1", "1e2"),
    ] {
        isolated(&format!("num-{tag}"))
            .args([
                "order",
                "place",
                "--account",
                "SMOKE",
                "--symbol",
                "SAMPL",
                "--side",
                "buy",
                "--qty",
                qty,
                "--limit",
                limit,
                "--tif",
                "day",
                "--dry-run",
                "--json",
            ])
            .assert()
            .code(2);
    }
}

#[test]
fn market_hours_are_validated_and_fractional_is_regular_hours_only() {
    isolated("hours-bogus")
        .args([
            "order",
            "place",
            "--account",
            "SMOKE",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "1",
            "--limit",
            "1.00",
            "--market-hours",
            "overnight",
            "--tif",
            "day",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stderr(contains("--market-hours"));
    isolated("hours-frac")
        .args([
            "order",
            "place",
            "--account",
            "SMOKE",
            "--symbol",
            "SAMPL",
            "--side",
            "sell",
            "--qty",
            "0.5",
            "--type",
            "market",
            "--market-hours",
            "extended_hours",
            "--tif",
            "day",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stderr(contains("regular_hours only"));
    isolated("hours-limit-ext")
        .args([
            "order",
            "place",
            "--account",
            "SMOKE",
            "--symbol",
            "SAMPL",
            "--side",
            "buy",
            "--qty",
            "1",
            "--limit",
            "1.00",
            "--market-hours",
            "extended_hours",
            "--tif",
            "day",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .stdout(contains("extended_hours"));
}

#[test]
fn option_contract_counts_are_whole_numbers() {
    for (tag, qty) in [
        ("frac", "1.5"),
        ("exp", "1e1"),
        ("inf", "inf"),
        ("zero", "0"),
    ] {
        isolated(&format!("optqty-{tag}"))
            .args([
                "option",
                "order",
                "place",
                "--account",
                "SMOKE",
                "--quantity",
                qty,
                "--instrument-id",
                "INST-1",
                "--position-effect",
                "open",
                "--side",
                "buy",
                "--type",
                "limit",
                "--price",
                "1.00",
                "--dry-run",
            ])
            .assert()
            .code(2);
    }
}

#[test]
fn a_blank_cancel_id_is_a_usage_error() {
    isolated("cancel-blank")
        .args(["order", "cancel", "  ", "--account", "SMOKE", "--dry-run"])
        .assert()
        .code(2)
        .stderr(contains("order id"));
}

/// Audit 2026-09-23 security #5: `self-update` installed an unverified
/// download re-signed with the trusted identity. It is gone.
#[test]
fn there_is_no_self_update() {
    rhap().arg("self-update").assert().code(2);
    let out = rhap().arg("--help").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(!text.contains("self-update"), "{text}");
}
