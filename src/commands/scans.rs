//! Scanner reads and writes. `list`/`filter-specs`/`run` are reads (a scan run
//! returns matches and changes no account state; the server annotates
//! `run_scan` `readOnlyHint: true`). The
//! `create`/`update-*` commands are guarded writes via
//! [`super::order::run_write`]. None are account-scoped.
//!
//! Schemas VERIFIED live 2026-08-27 (docs/api.md §7). Structured array fields
//! (`filters`, `columns`) are accepted as raw JSON.

use pk_cli_core::CliError;
use serde_json::Value;

use super::args::{json_array, run_read, Args};
use super::order::run_write;
use super::Ctx;

// ------------------------------------------------------------- reads / run

/// `scans list` → `get_scans`.
pub fn list(ctx: &Ctx) -> Result<(), CliError> {
    run_read(ctx, "scans", "get_scans", Args::new().build())
}

/// `scans filter-specs` → `get_scanner_filter_specs`.
pub fn filter_specs(ctx: &Ctx) -> Result<(), CliError> {
    run_read(
        ctx,
        "scanner-filter-specs",
        "get_scanner_filter_specs",
        Args::new().build(),
    )
}

/// `scans run --scan-id` → `run_scan`. A read (returns matches, changes
/// nothing; annotated `readOnlyHint: true`).
pub fn run(ctx: &Ctx, scan_id: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "scan-run",
        "run_scan",
        Args::new().str("scan_id", scan_id).build(),
    )
}

// ----------------------------------------------------------------- writes

/// `scans create [--scan-id --preset --title --filters <json> --columns <json>]`.
#[allow(clippy::too_many_arguments)]
pub fn create(
    ctx: &Ctx,
    scan_id: Option<&str>,
    preset: Option<&str>,
    title: Option<&str>,
    filters: Option<&str>,
    columns: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let filters = filters.map(|f| json_array("--filters", f)).transpose()?;
    let columns = columns.map(|c| json_array("--columns", c)).transpose()?;
    let args = Args::new()
        .opt("scan_id", scan_id)
        .opt("preset", preset)
        .opt("title", title)
        .opt_value("filters", filters)
        .opt_value("columns", columns)
        .build();
    run_write(
        ctx,
        "scan-create-dry-run",
        "scan-create",
        "create_scan",
        args,
        dry_run,
        force,
        "Create a scan?",
    )
}

/// `scans update-config --scan-id [--sorting-column --sorting-direction
/// --columns <json>]`.
pub fn update_config(
    ctx: &Ctx,
    scan_id: &str,
    sorting_column: Option<&str>,
    sorting_direction: Option<&str>,
    columns: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let columns = columns.map(|c| json_array("--columns", c)).transpose()?;
    let args = Args::new()
        .str("scan_id", scan_id)
        .opt("sorting_column", sorting_column)
        .opt("sorting_direction", sorting_direction)
        .opt_value("columns", columns)
        .build();
    run_write(
        ctx,
        "scan-update-config-dry-run",
        "scan-update-config",
        "update_scan_config",
        args,
        dry_run,
        force,
        &format!("Update config for scan {scan_id}?"),
    )
}

/// `scans update-filters --scan-id --filters <json>`.
pub fn update_filters(
    ctx: &Ctx,
    scan_id: &str,
    filters: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let filters: Value = json_array("--filters", filters)?;
    let args = Args::new()
        .str("scan_id", scan_id)
        .opt_value("filters", Some(filters))
        .build();
    run_write(
        ctx,
        "scan-update-filters-dry-run",
        "scan-update-filters",
        "update_scan_filters",
        args,
        dry_run,
        force,
        &format!("Update filters for scan {scan_id}?"),
    )
}
