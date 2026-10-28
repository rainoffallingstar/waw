mod backend;
mod capture;
mod cli;
mod commands;
mod holds;
mod text;
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
use text::*;
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
use commands::rank::*;
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
