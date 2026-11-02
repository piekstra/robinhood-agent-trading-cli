//! Watchlist reads and writes. Reads emit `<name>/v1` DTOs; writes reuse the
//! shared guard chain ([`super::order::run_write`]): `--dry-run` prints the
//! request and sends nothing, and a live run needs a typed confirmation
//! (exit 6 non-interactively without `--force`). None are account-scoped (their
//! schemas do not take `account_number`).
//!
//! Schemas VERIFIED live 2026-08-27 (docs/api.md §7).

use pk_cli_core::CliError;
use serde_json::Value;

use super::args::{run_read, Args};
use super::order::run_write;
use super::Ctx;

// ------------------------------------------------------------------ reads

/// `watchlists list` → `get_watchlists`.
pub fn list(ctx: &Ctx) -> Result<(), CliError> {
    run_read(ctx, "watchlists", "get_watchlists", Args::new().build())
}

/// `watchlists items --list-id` → `get_watchlist_items`.
pub fn items(ctx: &Ctx, list_id: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "watchlist-items",
        "get_watchlist_items",
        Args::new().str("list_id", list_id).build(),
    )
}

/// `watchlists popular` → `get_popular_watchlists`.
pub fn popular(ctx: &Ctx) -> Result<(), CliError> {
    run_read(
        ctx,
        "popular-watchlists",
        "get_popular_watchlists",
        Args::new().build(),
    )
}

// ----------------------------------------------------------------- writes

fn write(
    ctx: &Ctx,
    slug: &str,
    tool: &str,
    args: Value,
    dry_run: bool,
    force: bool,
    question: &str,
) -> Result<(), CliError> {
    run_write(
        ctx,
        &format!("{slug}-dry-run"),
        slug,
        tool,
        args,
        dry_run,
        force,
        question,
    )
}

/// `watchlists create --display-name [--icon-emoji --description]`.
pub fn create(
    ctx: &Ctx,
    display_name: &str,
    icon_emoji: Option<&str>,
    description: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("display_name", display_name)
        .opt("icon_emoji", icon_emoji)
        .opt("display_description", description)
        .build();
    write(
        ctx,
        "watchlist-create",
        "create_watchlist",
        args,
        dry_run,
        force,
        &format!("Create watchlist `{display_name}`?"),
    )
}

/// `watchlists update --list-id [--display-name --icon-emoji --description]`.
pub fn update(
    ctx: &Ctx,
    list_id: &str,
    display_name: Option<&str>,
    icon_emoji: Option<&str>,
    description: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("list_id", list_id)
        .opt("display_name", display_name)
        .opt("icon_emoji", icon_emoji)
        .opt("display_description", description)
        .build();
    write(
        ctx,
        "watchlist-update",
        "update_watchlist",
        args,
        dry_run,
        force,
        &format!("Update watchlist {list_id}?"),
    )
}

/// `watchlists add --list-id [--symbols --index-ids --currency-pair-ids]`.
pub fn add(
    ctx: &Ctx,
    list_id: &str,
    symbols: Option<&str>,
    index_ids: Option<&str>,
    currency_pair_ids: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("list_id", list_id)
        .opt_csv("symbols", symbols)
        .opt_csv("index_ids", index_ids)
        .opt_csv("currency_pair_ids", currency_pair_ids)
        .build();
    write(
        ctx,
        "watchlist-add",
        "add_to_watchlist",
        args,
        dry_run,
        force,
        &format!("Add to watchlist {list_id}?"),
    )
}

/// `watchlists remove --list-id [--symbols --index-ids --currency-pair-ids]`.
pub fn remove(
    ctx: &Ctx,
    list_id: &str,
    symbols: Option<&str>,
    index_ids: Option<&str>,
    currency_pair_ids: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("list_id", list_id)
        .opt_csv("symbols", symbols)
        .opt_csv("index_ids", index_ids)
        .opt_csv("currency_pair_ids", currency_pair_ids)
        .build();
    write(
        ctx,
        "watchlist-remove",
        "remove_from_watchlist",
        args,
        dry_run,
        force,
        &format!("Remove from watchlist {list_id}?"),
    )
}

/// `watchlists add-option --option-ids [--position-type]`.
pub fn add_option(
    ctx: &Ctx,
    option_ids: &str,
    position_type: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("option_ids", option_ids)
        .opt("position_type", position_type)
        .build();
    write(
        ctx,
        "watchlist-add-option",
        "add_option_to_watchlist",
        args,
        dry_run,
        force,
        "Add option(s) to the watchlist?",
    )
}

/// `watchlists remove-option --option-ids [--position-type]`.
pub fn remove_option(
    ctx: &Ctx,
    option_ids: &str,
    position_type: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("option_ids", option_ids)
        .opt("position_type", position_type)
        .build();
    write(
        ctx,
        "watchlist-remove-option",
        "remove_option_from_watchlist",
        args,
        dry_run,
        force,
        "Remove option(s) from the watchlist?",
    )
}

/// `watchlists follow --list-id`.
pub fn follow(ctx: &Ctx, list_id: &str, dry_run: bool, force: bool) -> Result<(), CliError> {
    write(
        ctx,
        "watchlist-follow",
        "follow_watchlist",
        Args::new().str("list_id", list_id).build(),
        dry_run,
        force,
        &format!("Follow watchlist {list_id}?"),
    )
}

/// `watchlists unfollow --list-id`.
pub fn unfollow(ctx: &Ctx, list_id: &str, dry_run: bool, force: bool) -> Result<(), CliError> {
    write(
        ctx,
        "watchlist-unfollow",
        "unfollow_watchlist",
        Args::new().str("list_id", list_id).build(),
        dry_run,
        force,
        &format!("Unfollow watchlist {list_id}?"),
    )
}
