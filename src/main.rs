mod backend;
mod capture;
mod cli;
mod commands;
mod holds;
mod config;
mod elevation;
mod discovery;
mod exec;
mod json;
mod model;
mod parse;
mod render;
#[cfg(windows)]
mod win32;

use json::{JsonValue, escape_json_string, parse_json};
use discovery::*;
use holds::Holds;
use model::*;
use backend::Backend;
#[allow(unused_imports)]
use cli::*;
use commands::backend::*;
use commands::export::*;
use commands::hold::*;
use commands::install::*;
use commands::collect::*;
use commands::update::*;
use commands::query::*;
use commands::select::*;
use config::*;
#[allow(unused_imports)]
use elevation::*;
#[allow(unused_imports)]
use exec::*;
#[allow(unused_imports)]
use capture::{decode_capture_text, read_capture_log, unique_capture_base};
use parse::*;
use render::*;
#[cfg(windows)]
use win32::{
    CloseHandle, ERROR_CANCELLED, GetExitCodeProcess, Handle, SEE_MASK_NOCLOSEPROCESS,
    ShellExecuteExW, ShellExecuteInfoW, TerminateProcess, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    WaitForSingleObject,
};
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt::{self, Display};
use std::fs;
use std::io::ErrorKind;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const ELEVATED_STEP_MARKER: &str = "WAW_ELEVATED_STEP:";
const ELEVATED_SUCCESS_MARKER: &str = "WAW_ELEVATED_SUCCESS:";
const ELEVATED_FAILURE_MARKER: &str = "WAW_ELEVATED_FAILURE:";
const ELEVATED_BOOTSTRAP_FAILURE_MARKER: &str = "WAW_ELEVATED_BOOTSTRAP_FAILURE:";
const WINGET_NO_APPLICABLE_UPGRADE_EXIT_CODE: i32 = -1978335189;
const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 180;

/// `0` means "wait forever".
fn timeout_from_seconds(seconds: u64) -> Option<Duration> {
    match seconds {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    }
}



fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let cli = Cli::parse(env::args().skip(1))?;

    if cli.show_version {
        println!("{APP_NAME} {APP_VERSION}");
        return Ok(ExitCode::SUCCESS);
    }

    if cli.show_help {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let mut loaded_config = LoadedConfig::load(cli.config_path.as_deref())?;
    let backend_assume_yes = cli.assume_yes.unwrap_or(loaded_config.config.assume_yes);
    let backend_auto_elevate = cli
        .auto_elevate
        .unwrap_or(loaded_config.config.auto_elevate);

    if let Some(code) = maybe_relaunch_self_elevated(&cli, &loaded_config.config)? {
        return Ok(code);
    }

    if matches!(
        cli.command,
        Subcommand::Backends | Subcommand::Backend { action: _ }
    ) {
        return handle_backend_command(
            &cli.command,
            &mut loaded_config,
            cli.dry_run,
            cli.json,
            backend_assume_yes,
            backend_auto_elevate,
        );
    }

    if cli.json && !command_supports_json(&cli.command) {
        return Err(
            "`--json` is currently supported only for `backends`, `backend ...`, and `show`."
                .to_string(),
        );
    }

    let config = &loaded_config.config;
    let selected_backend = cli.backend.or(config.backend);
    let runtime = runtime_from(config, &cli);

    // One exhaustive match instead of a chain of `if let` checks: adding a
    // Subcommand variant now fails to compile until it is routed here.
    match &cli.command {
        Subcommand::Update => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_update_all(backends, cli.dry_run, &runtime);
        }
        Subcommand::Upgrade { packages } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            if packages.is_empty() {
                return run_interactive_upgrade(
                    backends,
                    &runtime,
                    cli.dry_run,
                    cli.config_path.as_deref(),
                );
            }
            // Named packages fall through to the single-backend path.
        }
        Subcommand::Bucket { action } => {
            return run_bucket_command(action.clone(), config, cli.dry_run, &runtime);
        }
        Subcommand::Export { path } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_export(backends, path.clone(), &runtime);
        }
        Subcommand::Import { path } => {
            return run_import(path, cli.dry_run, selected_backend, config, &runtime);
        }
        Subcommand::Search { query, .. } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_search(backends, query, &runtime, cli.json);
        }
        Subcommand::Install {
            mode: InstallMode::Search(query),
        } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_interactive_install(backends, query, &runtime, cli.dry_run);
        }
        Subcommand::Remove {
            mode: RemoveMode::Search(query),
        } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_interactive_remove(backends, query, &runtime, cli.dry_run);
        }
        Subcommand::List { upgradable } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_list(backends, *upgradable, &runtime, cli.dry_run, cli.json);
        }
        Subcommand::Show { package } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_show(backends, package, &runtime, cli.dry_run, cli.json);
        }
        Subcommand::Hold { list: true, .. } => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_hold_list(backends, &runtime, cli.config_path.as_deref());
        }
        Subcommand::Hold {
            packages,
            enable,
            list: false,
        } => {
            let backend = resolve_single_backend(selected_backend, config)?;
            return run_hold(
                backend,
                packages,
                *enable,
                &runtime,
                cli.dry_run,
                cli.config_path.as_deref(),
            );
        }
        Subcommand::Install {
            mode: InstallMode::Exact(_),
        }
        | Subcommand::Remove {
            mode: RemoveMode::Exact(_),
        }
        | Subcommand::Backends
        | Subcommand::Backend { .. }
        | Subcommand::Help => {
            // Routed by the single-backend path below.
        }
    }

    let backend = match selected_backend {
        Some(backend) => {
            if !config.is_backend_enabled(backend) {
                return Err(format!(
                    "backend {backend} is disabled in config. Use `backend enable {backend}` first."
                ));
            }
            backend
        }
        None => Backend::detect(config)?,
    };

    if !cfg!(windows) && !cli.dry_run {
        eprintln!(
            "warning: {APP_NAME} is intended to run on Windows; execution may fail on this host"
        );
    }

    // Named upgrades are the one mutating path that can target a package waw
    // itself is holding (pip/npm have no native pin to refuse for us).
    let is_upgrade = matches!(cli.command, Subcommand::Upgrade { .. });
    let command = match &cli.command {
        Subcommand::Upgrade { packages } => Subcommand::Upgrade {
            packages: filter_held_upgrade_targets(
                backend,
                packages.clone(),
                cli.config_path.as_deref(),
            )?,
        },
        other => other.clone(),
    };

    let invocations = backend.plan(&command, &runtime)?;
    execute_invocations(invocations, cli.dry_run, is_upgrade, &runtime)
}

fn runtime_from<'a>(config: &'a Config, cli: &Cli) -> RuntimeSettings<'a> {
    RuntimeSettings {
        assume_yes: cli.assume_yes.unwrap_or(config.assume_yes),
        auto_elevate: cli.auto_elevate.unwrap_or(config.auto_elevate),
        command_timeout: timeout_from_seconds(config.command_timeout_seconds),
        deep_search: matches!(
            cli.command,
            Subcommand::Search { deep: true, .. }
        ),
        config,
    }
}

fn maybe_relaunch_self_elevated(cli: &Cli, config: &Config) -> Result<Option<ExitCode>, String> {
    if !cfg!(windows) || cli.dry_run {
        return Ok(None);
    }

    if cli.json
        && !matches!(
            cli.command,
            Subcommand::Backends | Subcommand::Backend { action: _ }
        )
        && !command_supports_json(&cli.command)
    {
        return Ok(None);
    }

    let runtime = runtime_from(config, cli);
    if !runtime.auto_elevate || is_process_elevated() {
        return Ok(None);
    }

    let selected_backend = cli.backend.or(config.backend);
    if !command_requires_process_elevation(&cli.command, selected_backend, &runtime)? {
        return Ok(None);
    }

    println!("requesting administrator privileges...");
    let capture = run_current_process_elevated()?;
    emit_successful_command_output(&capture.stdout, &capture.stderr);
    Ok(Some(ExitCode::from(capture.status_code as u8)))
}

fn command_requires_process_elevation(
    command: &Subcommand,
    selected_backend: Option<Backend>,
    runtime: &RuntimeSettings<'_>,
) -> Result<bool, String> {
    match command {
        Subcommand::Update => {
            let backends = resolve_auto_backends(selected_backend, runtime.config)?;
            Ok(backends_require_elevation(&backends, runtime))
        }
        Subcommand::Upgrade { packages } if packages.is_empty() => Ok(false),
        Subcommand::Install {
            mode: InstallMode::Search(_),
        } => Ok(false),
        Subcommand::Install {
            mode: InstallMode::Exact(_),
        }
        | Subcommand::Remove {
            mode: RemoveMode::Search(_),
        } => Ok(false),
        Subcommand::Remove {
            mode: RemoveMode::Exact(_),
        }
        | Subcommand::Upgrade { .. }
        | Subcommand::Hold { .. } => {
            let backend = resolve_single_backend(selected_backend, runtime.config)?;
            Ok(backend.requires_elevation_for_mutation(runtime))
        }
        Subcommand::Backend {
            action: BackendAction::Install { backend, .. },
        } => Ok(backend
            .install_invocation(runtime.assume_yes)
            .map(|invocation| invocation.requires_elevation)
            .unwrap_or(false)),
        _ => Ok(false),
    }
}

fn backends_require_elevation(backends: &[Backend], runtime: &RuntimeSettings<'_>) -> bool {
    backends
        .iter()
        .copied()
        .any(|backend| backend.requires_elevation_for_mutation(runtime))
}

fn resolve_single_backend(
    selected_backend: Option<Backend>,
    config: &Config,
) -> Result<Backend, String> {
    match selected_backend {
        Some(backend) => {
            if !config.is_backend_enabled(backend) {
                return Err(format!(
                    "backend {backend} is disabled in config. Use `backend enable {backend}` first."
                ));
            }
            Ok(backend)
        }
        None => Backend::detect(config),
    }
}

/// Multi-backend commands downgrade a single backend failure to a warning so
/// one broken tool does not hide the others. When *every* backend fails there
/// is nothing to report, so that case must surface as a real error -- otherwise
/// a script cannot tell "all tools broken" from "nothing installed".
fn all_backends_failed(
    noun: &str,
    attempted: usize,
    failures: &[String],
) -> Option<String> {
    if attempted == 0 || failures.len() < attempted {
        return None;
    }
    Some(format!(
        "failed to {noun}: all {} backend(s) failed ({})",
        attempted,
        failures.join("; ")
    ))
}

fn resolve_auto_backends(
    selected_backend: Option<Backend>,
    config: &Config,
) -> Result<Vec<Backend>, String> {
    if let Some(backend) = selected_backend {
        if !config.is_backend_enabled(backend) {
            return Err(format!(
                "backend {backend} is disabled in config. Use `backend enable {backend}` first."
            ));
        }
        return Ok(vec![backend]);
    }

    let backends = enabled_available_backends(config);
    if backends.is_empty() {
        Err("no enabled backend was found in PATH. Enable a backend or install winget, scoop, choco, npm, or pip.".to_string())
    } else {
        Ok(backends)
    }
}

fn enabled_available_backends(config: &Config) -> Vec<Backend> {
    collect_backend_statuses(config)
        .into_iter()
        .filter(|status| status.enabled && status.available)
        .map(|status| status.backend)
        .collect()
}

fn format_elapsed(elapsed: Duration) -> String {
    if elapsed.as_secs() >= 60 {
        let minutes = elapsed.as_secs() / 60;
        let seconds = elapsed.as_secs() % 60;
        format!("{minutes}m{seconds:02}s")
    } else if elapsed.as_secs() >= 1 {
        format!(
            "{}.{:01}s",
            elapsed.as_secs(),
            elapsed.subsec_millis() / 100
        )
    } else {
        format!("{}ms", elapsed.as_millis())
    }
}


/// `upgrade` tracks installed packages, so a version pin there would mean
/// "move to this exact version" rather than "install this exact version".
/// Keep the user's spec text so `pkg@1.2.3` still upgrades to 1.2.3.
fn upgrade_specs(packages: &[String]) -> Vec<PackageSpec> {
    packages.iter().map(|raw| PackageSpec::parse(raw)).collect()
}

fn package_upgrade_candidate(entry: PackageListEntry) -> SearchCandidate {
    SearchCandidate {
        backend: entry.backend,
        label: entry.name,
        install_id: entry.package_id,
        version: Some(match entry.available_version {
            Some(latest) => format!("{} -> {}", entry.current_version, latest),
            None => entry.current_version,
        }),
        source: None,
    }
}

fn update_backend_summary(backend: Backend) -> String {
    match backend {
        Backend::Winget => "winget: refreshing sources".to_string(),
        Backend::Scoop => "scoop: refreshing buckets".to_string(),
        Backend::Chocolatey => "choco: no dedicated refresh step".to_string(),
        Backend::Npm => "npm: no dedicated refresh step".to_string(),
        Backend::Pip => "pip: no dedicated refresh step".to_string(),
    }
}

fn print_list_rows(rows: &[PackageListEntry], upgradable: bool) {
    let backend_width = rows
        .iter()
        .map(|row| row.backend.to_string().len())
        .max()
        .unwrap_or(7)
        .max("Backend".len());
    let package_width = rows
        .iter()
        .map(|row| row.name.len())
        .max()
        .unwrap_or(7)
        .max("Package".len());
    let current_width = rows
        .iter()
        .map(|row| row.current_version.len())
        .max()
        .unwrap_or(7)
        .max(if upgradable {
            "Current".len()
        } else {
            "Version".len()
        });

    println!();
    if upgradable {
        let latest_width = rows
            .iter()
            .map(|row| row.available_version.as_deref().unwrap_or("-").len())
            .max()
            .unwrap_or(6)
            .max("Latest".len());
        println!(
            "{:<backend_width$}  {:<package_width$}  {:<current_width$}  {:<latest_width$}",
            "Backend", "Package", "Current", "Latest"
        );
        println!(
            "{:-<backend_width$}  {:-<package_width$}  {:-<current_width$}  {:-<latest_width$}",
            "", "", "", ""
        );
        for row in rows {
            println!(
                "{:<backend_width$}  {:<package_width$}  {:<current_width$}  {:<latest_width$}",
                row.backend,
                row.name,
                row.current_version,
                row.available_version.as_deref().unwrap_or("-"),
            );
        }
    } else {
        println!(
            "{:<backend_width$}  {:<package_width$}  {:<current_width$}",
            "Backend", "Package", "Version"
        );
        println!(
            "{:-<backend_width$}  {:-<package_width$}  {:-<current_width$}",
            "", "", ""
        );
        for row in rows {
            println!(
                "{:<backend_width$}  {:<package_width$}  {:<current_width$}",
                row.backend, row.name, row.current_version,
            );
        }
    }
}

fn sort_list_rows(mut rows: Vec<PackageListEntry>) -> Vec<PackageListEntry> {
    rows.sort_by(|left, right| {
        (
            normalize_search_text(&left.name),
            left.backend.to_string(),
            normalize_search_text(&left.current_version),
            normalize_search_text(left.available_version.as_deref().unwrap_or("")),
        )
            .cmp(&(
                normalize_search_text(&right.name),
                right.backend.to_string(),
                normalize_search_text(&right.current_version),
                normalize_search_text(right.available_version.as_deref().unwrap_or("")),
            ))
    });
    rows
}

fn package_list_entry_matches_query(entry: &PackageListEntry, query: &str) -> bool {
    let query_terms: Vec<&str> = query.split_whitespace().collect();
    if query_terms.is_empty() {
        return true;
    }

    let available_version = entry.available_version.as_deref().unwrap_or("");
    let fields = [
        normalize_search_text(&entry.name),
        normalize_search_text(&entry.package_id),
        normalize_search_text(&entry.backend.to_string()),
        normalize_search_text(&entry.current_version),
        normalize_search_text(available_version),
        normalize_search_text(&format!(
            "{} {} {} {} {}",
            entry.name, entry.package_id, entry.backend, entry.current_version, available_version
        )),
    ];
    let compact_fields = fields
        .iter()
        .map(|field| compact_search_text(field))
        .collect::<Vec<_>>();

    query_terms.iter().all(|term| {
        let compact_term = compact_search_text(term);
        fields
            .iter()
            .zip(compact_fields.iter())
            .any(|(field, compact_field)| {
                field.contains(term)
                    || (!compact_term.is_empty() && compact_field.contains(&compact_term))
            })
    })
}

fn package_list_entry_candidate(entry: PackageListEntry) -> SearchCandidate {
    SearchCandidate {
        backend: entry.backend,
        label: entry.name,
        install_id: entry.package_id,
        version: Some(entry.current_version),
        source: None,
    }
}

fn remove_target_from_candidate(candidate: &SearchCandidate) -> RemoveTarget {
    RemoveTarget {
        package: candidate.install_id.clone(),
        version: candidate.version.clone(),
    }
}

fn command_works(program: &str, args: &[String]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn dedupe_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    dedupe_search_candidates(candidates)
}

fn dedupe_search_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing: &SearchCandidate| {
            existing.backend == candidate.backend
                && existing
                    .install_id
                    .eq_ignore_ascii_case(&candidate.install_id)
                && existing.source == candidate.source
        }) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn dedupe_installed_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing: &SearchCandidate| {
            existing.backend == candidate.backend
                && existing
                    .install_id
                    .eq_ignore_ascii_case(&candidate.install_id)
                && existing.label.eq_ignore_ascii_case(&candidate.label)
                && existing.version == candidate.version
        }) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn sort_search_candidates(
    mut candidates: Vec<SearchCandidate>,
    query: &str,
    backends: &[Backend],
) -> Vec<SearchCandidate> {
    let query = normalize_search_text(query);

    // pip already ranks its own results: the prober compares publication dates
    // so a maintained `beautifulsoup4` outranks the abandoned `BeautifulSoup`.
    // Re-sorting here with the generic prefix heuristic would undo that, since
    // "BeautifulSoup" is an exact match and would win on rank alone.
    if candidates.iter().all(|candidate| candidate.backend == Backend::Pip) {
        return candidates;
    }

    candidates.sort_by(|left, right| {
        let left_key = search_sort_key(left, &query, backends);
        let right_key = search_sort_key(right, &query, backends);
        left_key.cmp(&right_key)
    });
    candidates
}

fn search_sort_key(
    candidate: &SearchCandidate,
    query: &str,
    backends: &[Backend],
) -> (u8, usize, u8, usize, usize, String, String) {
    let label = normalize_search_text(&candidate.label);
    let install_id = normalize_search_text(&candidate.install_id);
    (
        search_match_rank(&label, &install_id, query),
        backend_rank_for_search(candidate.backend, backends),
        if candidate.version.is_some() { 0 } else { 1 },
        install_id.len(),
        label.len(),
        label,
        install_id,
    )
}

fn search_match_rank(label: &str, install_id: &str, query: &str) -> u8 {
    if query.is_empty() {
        return 3;
    }

    let compact_query = compact_search_text(query);
    let compact_label = compact_search_text(label);
    let compact_install_id = compact_search_text(install_id);

    if label == query
        || install_id == query
        || (!compact_query.is_empty()
            && (compact_label == compact_query || compact_install_id == compact_query))
    {
        0
    } else if label.starts_with(query)
        || install_id.starts_with(query)
        || (!compact_query.is_empty()
            && (compact_label.starts_with(&compact_query)
                || compact_install_id.starts_with(&compact_query)))
    {
        1
    } else if label.contains(query)
        || install_id.contains(query)
        || (!compact_query.is_empty()
            && (compact_label.contains(&compact_query)
                || compact_install_id.contains(&compact_query)))
    {
        2
    } else {
        3
    }
}

fn backend_rank_for_search(backend: Backend, backends: &[Backend]) -> usize {
    backends
        .iter()
        .position(|candidate| *candidate == backend)
        .unwrap_or(backends.len())
}

fn normalize_search_text(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn compact_search_text(value: &str) -> String {
    normalize_search_text(value)
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn extract_json_string_value_after_key(input: &str, start: usize, key: &str) -> Option<String> {
    input[start..]
        .find(key)
        .and_then(|offset| extract_json_string_value(input, start + offset))
}

fn extract_json_string_value(input: &str, key_pos: usize) -> Option<String> {
    let after_colon = input[key_pos..].find(':')? + key_pos + 1;
    let first_quote = input[after_colon..].find('"')? + after_colon + 1;
    let mut escaped = false;
    let mut value = String::new();
    for ch in input[first_quote..].chars() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(value);
        }
        value.push(ch);
    }
    None
}

fn starts_with_version_char(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
}

fn looks_like_version(value: &str) -> bool {
    starts_with_version_char(value.trim_matches(|ch| ch == '(' || ch == ')' || ch == ':'))
}

fn normalize_list_version(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch| ch == '(' || ch == ')' || ch == ':')
        .to_string()
}

fn looks_like_url(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("http://") || value.starts_with("https://")
}

fn find_char_index(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .find(needle)
        .map(|byte_index| haystack[..byte_index].chars().count())
}

fn slice_chars(value: &str, start: usize, end: usize) -> String {
    value
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn parse_string(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.starts_with('"') {
        if !trimmed.ends_with('"') || trimmed.len() < 2 {
            return Err(format!("invalid quoted string: {trimmed}"));
        }
        Ok(trimmed[1..trimmed.len() - 1].replace("\\\"", "\""))
    } else {
        Ok(trimmed.to_string())
    }
}

/// Parses a TOML-ish inline array of strings: `["main", "extras"]`.
///
/// A bare scalar is also accepted and treated as a one-element list, so
/// `scoop_buckets = "extras"` does not fail.
fn parse_string_list(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'));

    let Some(inner) = inner else {
        // A value that starts a list but never closes it is a typo, not a
        // scalar. Rejecting it beats silently treating `["main", "extras"` as
        // one bucket named that.
        if trimmed.starts_with('[') {
            return Err(format!("unterminated array value: {trimmed}"));
        }
        if trimmed.contains(']') {
            return Err(format!("unexpected ']' in value: {trimmed}"));
        }
        return Ok(vec![parse_string(trimmed)?]);
    };

    let mut items = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let value = parse_string(part)?;
        if !value.is_empty() && !items.contains(&value) {
            items.push(value);
        }
    }
    Ok(items)
}

fn render_toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("invalid boolean value: {raw}")),
    }
}

fn default_config_path() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join(APP_NAME).join("config.toml"))
    } else if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path).join(APP_NAME).join("config.toml"))
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .map(|path| path.join(".config").join(APP_NAME).join("config.toml"))
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }

    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '*' | '/' | '\\')
    }) {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn command_supports_json(command: &Subcommand) -> bool {
    matches!(
        command,
        Subcommand::Show { .. } | Subcommand::List { .. } | Subcommand::Search { .. }
    )
}

fn print_help() {
    let config_hint = default_config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<system-dependent>".to_string());

    println!(
        "\
{APP_NAME} {APP_VERSION}

Usage:
{APP_NAME} [--backend <winget|scoop|choco|npm|pip>] [--config <path>] [--dry-run] [--json] [--interactive] [--no-elevate] <command> [args...]

Commands:
  update                   Refresh package metadata across all enabled+available backends in auto mode
  upgrade [pkg...]         Preview available upgrades and choose packages interactively, or upgrade only the named packages on the selected backend
  install <query...>       Search interactively across the selected/auto backends, then choose packages
  install --exact <pkg...> Install one or more packages directly on the selected backend
                           Pin a version with pkg@1.2.3 or pkg==1.2.3 (pip)
  remove <query...>        Search installed packages across the selected/auto backends, then choose packages to uninstall
  remove --exact <pkg...>  Uninstall one or more packages directly on the selected backend
  hold [--off] <pkg...>    Add or remove an upgrade hold
  hold --list              Show every held package across enabled backends
  search [--deep] <query...>
                           Search the selected backend or all enabled+available backends
                           --deep (pip only): search the full PyPI index (~10 MB first run, cached)
                           Note: PyPI has no search API; pip results are name probes only
  list [--upgradable]      List packages in a normalized table, using the selected backend or all enabled+available backends in auto mode
  show <pkg>               Show normalized package details from the selected backend, or probe all enabled+available backends in auto mode
  export [--file <path>]   Emit an installable manifest of installed packages
  import <path>            Install packages listed in a waw or winget export manifest
  backends                 Show detected backend availability and enabled state
  backend list             Same as `backends`
  backend enable <name>    Enable a backend in config
  backend disable <name>   Disable a backend in config
  backend install <name>   Run a bootstrap install command when supported on this host
  backend default <name>   Set the default backend and enable it if needed
  backend default auto     Clear the explicit default and return to auto detection
  bucket [list]            List Scoop buckets and show which one is the default

Options:
  -b, --backend <name>     Select backend explicitly
      --config <path>      Load config from a custom path
      --dry-run            Print backend commands without executing them
      --json               Emit JSON for `backends`, `backend ...`, `show`, `list`, and `search` (accepted before or after the command)
  -y, --yes                Force non-interactive execution (default)
      --interactive        Allow backend commands to prompt when supported
      --elevate            Force automatic Windows elevation attempts
      --no-elevate         Disable automatic Windows elevation attempts
  -h, --help               Show this help text
  -V, --version            Show version

Environment:
  WAW_FORCE_INTERACTIVE=1   Allow the interactive selector even when stdin is not a TTY
  WAW_<BACKEND>_CMD         Override a backend executable (e.g. WAW_WINGET_CMD)

Default config path:
  {config_hint}

Examples:
  {APP_NAME} update
  {APP_NAME} --interactive upgrade
  {APP_NAME} upgrade
  {APP_NAME} --backend winget upgrade
  {APP_NAME} install git
  {APP_NAME} install --exact Git.Git
  {APP_NAME} install --exact Git.Git@2.43.0
  {APP_NAME} remove git
  {APP_NAME} remove --exact Git.Git
  {APP_NAME} hold git
  {APP_NAME} hold --list
  {APP_NAME} hold --off Git.Git
  {APP_NAME} search powertoys
  {APP_NAME} list --upgradable
  {APP_NAME} show Git.Git
  {APP_NAME} export --file packages.json
  {APP_NAME} import packages.json
  {APP_NAME} bucket list
  {APP_NAME} backends
  {APP_NAME} --json backends
  {APP_NAME} --json show pip
  {APP_NAME} backend disable choco
  {APP_NAME} backend install npm --enable
  {APP_NAME} backend default pip
  {APP_NAME} --backend choco install git
  {APP_NAME} --backend npm install typescript
  {APP_NAME} --backend pip install requests
"
    );
}

#[cfg(test)]
mod tests;
