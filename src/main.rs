mod backend;
mod capture;
mod cli;
mod config;
mod elevation;
mod exec;
mod json;
mod model;
mod parse;
mod render;
#[cfg(windows)]
mod win32;

use json::{JsonValue, escape_json_string, parse_json};
use model::*;
use backend::Backend;
#[allow(unused_imports)]
use cli::*;
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

    match &cli.command {
        Subcommand::Update if selected_backend.is_none() => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_update_all(backends, cli.dry_run, &runtime);
        }
        Subcommand::Upgrade { packages } if packages.is_empty() => {
            let backends = resolve_auto_backends(selected_backend, config)?;
            return run_interactive_upgrade(backends, &runtime, cli.dry_run);
        }
        _ => {}
    }

    if let Subcommand::Bucket { action } = &cli.command {
        return run_bucket_command(action.clone(), config, cli.dry_run, &runtime);
    }

    if let Subcommand::Export { path } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_export(backends, path.clone(), &runtime);
    }

    if let Subcommand::Import { path } = &cli.command {
        return run_import(path, cli.dry_run, selected_backend, config, &runtime);
    }

    if let Subcommand::Search { query, .. } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_search(backends, query, &runtime, cli.json);
    }

    if let Subcommand::Install {
        mode: InstallMode::Search(query),
    } = &cli.command
    {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_interactive_install(backends, query, &runtime, cli.dry_run);
    }

    if let Subcommand::Remove {
        mode: RemoveMode::Search(query),
    } = &cli.command
    {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_interactive_remove(backends, query, &runtime, cli.dry_run);
    }

    if let Subcommand::List { upgradable } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_list(backends, *upgradable, &runtime, cli.dry_run, cli.json);
    }

    if let Subcommand::Show { package } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_show(backends, package, &runtime, cli.dry_run, cli.json);
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

    let invocations = backend.plan(&cli.command, &runtime)?;
    execute_invocations(
        invocations,
        cli.dry_run,
        matches!(cli.command, Subcommand::Upgrade { .. }),
        &runtime,
    )
}

fn handle_backend_command(
    command: &Subcommand,
    loaded_config: &mut LoadedConfig,
    dry_run: bool,
    json: bool,
    assume_yes: bool,
    auto_elevate: bool,
) -> Result<ExitCode, String> {
    match command {
        Subcommand::Backends
        | Subcommand::Backend {
            action: BackendAction::List,
        } => {
            if json {
                println!("{}", render_backend_statuses_json(&loaded_config.config));
            } else {
                print_backend_statuses(&loaded_config.config);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Enable { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            if dry_run {
                if !json {
                    println!("dry-run: would enable backend: {backend}");
                }
            } else {
                loaded_config.config.set_backend_enabled(*backend, true);
                loaded_config.save()?;
                if !json {
                    println!("Enabled backend: {backend}");
                }
            }
            preview.set_backend_enabled(*backend, true);
            if json {
                println!("{}", render_backend_status_json(*backend, &preview));
            } else {
                print_backend_status(*backend, &preview);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Disable { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            if dry_run {
                if !json {
                    println!("dry-run: would disable backend: {backend}");
                }
            } else {
                loaded_config.config.set_backend_enabled(*backend, false);
                loaded_config.save()?;
                if !json {
                    println!("Disabled backend: {backend}");
                }
            }
            preview.set_backend_enabled(*backend, false);
            if json {
                println!("{}", render_backend_status_json(*backend, &preview));
            } else {
                print_backend_status(*backend, &preview);
            }
            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Install { backend, enable },
        } => {
            if !backend.supported_on_host() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "unsupported",
                            None
                        )
                    );
                } else {
                    println!("Backend {backend} is not supported on this host.");
                    println!("Hint: {}", backend.install_hint());
                }
                return Ok(ExitCode::SUCCESS);
            }

            if !backend.supports_bootstrap_install_on_host() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "bootstrap_unsupported",
                            None
                        )
                    );
                } else {
                    println!(
                        "Automatic bootstrap install for {backend} is not supported on this host."
                    );
                    println!("Hint: {}", backend.install_hint());
                }
                return Ok(ExitCode::SUCCESS);
            }

            if backend.is_available() {
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            true,
                            *enable,
                            dry_run,
                            "already_available",
                            None
                        )
                    );
                } else {
                    println!("Backend {backend} is already available.");
                }
            } else if let Some(invocation) = backend.install_invocation(assume_yes) {
                if !json {
                    println!("Bootstrap command for {backend}:");
                }
                execute_invocations(
                    vec![invocation],
                    dry_run,
                    false,
                    &RuntimeSettings {
                        assume_yes,
                        auto_elevate,
                        command_timeout: timeout_from_seconds(
                            loaded_config.config.command_timeout_seconds,
                        ),
                        deep_search: false,
                        config: &loaded_config.config,
                    },
                )?;
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "bootstrap_requested",
                            backend.install_invocation(assume_yes).as_ref()
                        )
                    );
                }
            } else if json {
                println!(
                    "{}",
                    render_backend_install_json(
                        *backend,
                        false,
                        *enable,
                        dry_run,
                        "no_bootstrap",
                        None
                    )
                );
            } else {
                println!("Automatic install is not implemented for {backend}.");
                println!("Hint: {}", backend.install_hint());
            }

            if *enable {
                if dry_run {
                    if !json {
                        println!("dry-run: would enable backend after install request: {backend}");
                    }
                } else {
                    loaded_config.config.set_backend_enabled(*backend, true);
                    loaded_config.save()?;
                    if !json {
                        println!("Enabled backend after install request: {backend}");
                    }
                }
            }

            Ok(ExitCode::SUCCESS)
        }
        Subcommand::Backend {
            action: BackendAction::Default { backend },
        } => {
            let mut preview = loaded_config.config.clone();
            match backend {
                Some(backend) => {
                    if !preview.is_backend_enabled(*backend) {
                        preview.set_backend_enabled(*backend, true);
                    }
                    preview.backend = Some(*backend);
                    if dry_run {
                        if !json {
                            println!("dry-run: would set default backend: {backend}");
                        }
                    } else {
                        if !loaded_config.config.is_backend_enabled(*backend) {
                            loaded_config.config.set_backend_enabled(*backend, true);
                        }
                        loaded_config.config.backend = Some(*backend);
                        loaded_config.save()?;
                        if !json {
                            println!("Set default backend: {backend}");
                        }
                    }
                }
                None => {
                    preview.backend = None;
                    if dry_run {
                        if !json {
                            println!(
                                "dry-run: would clear explicit default backend and return to auto detection"
                            );
                        }
                    } else {
                        loaded_config.config.backend = None;
                        loaded_config.save()?;
                        if !json {
                            println!("Cleared explicit default backend. Auto detection is active.");
                        }
                    }
                }
            }
            if json {
                println!("{}", render_backend_statuses_json(&preview));
            } else {
                print_backend_statuses(&preview);
            }

            Ok(ExitCode::SUCCESS)
        }
        _ => Ok(ExitCode::SUCCESS),
    }
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

fn run_interactive_install(
    backends: Vec<Backend>,
    query: &str,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let candidates = collect_search_candidates(&backends, query, runtime, false)?;
    if candidates.is_empty() {
        println!("No matching packages found.");
        return Ok(ExitCode::SUCCESS);
    }

    print_candidates(&candidates);
    if dry_run {
        println!(
            "dry-run: candidate search executed, but install commands will only be printed after selection."
        );
    }

    let selected_indices = match choose_candidates(
        "install",
        &candidates,
        runtime.assume_yes,
    )? {
        Some(indices) => indices,
        None => {
            println!("No selection made. Nothing to install.");
            return Ok(ExitCode::SUCCESS);
        }
    };

    let mut invocations = Vec::new();
    for backend in backends {
        let packages: Vec<String> = selected_indices
            .iter()
            .map(|index| &candidates[*index - 1])
            .filter(|candidate| candidate.backend == backend)
            .map(|candidate| candidate.install_id.clone())
            .collect();
        if !packages.is_empty() {
            let specs: Vec<PackageSpec> = packages
                .iter()
                .map(|id| PackageSpec::without_version(id))
                .collect();
            invocations.extend(backend.plan_install(&specs, runtime));
        }
    }

    execute_invocations(invocations, dry_run, false, runtime)
}

fn run_interactive_remove(
    backends: Vec<Backend>,
    query: &str,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let candidates = collect_installed_candidates(&backends, query, runtime)?;
    if candidates.is_empty() {
        println!("No matching installed packages found.");
        return Ok(ExitCode::SUCCESS);
    }

    print_candidates(&candidates);
    if dry_run {
        println!(
            "dry-run: installed-package search executed, but uninstall commands will only be printed after selection."
        );
    }

    let selected_indices = match choose_candidates(
        "remove",
        &candidates,
        runtime.assume_yes,
    )? {
        Some(indices) => indices,
        None => {
            println!("No selection made. Nothing to remove.");
            return Ok(ExitCode::SUCCESS);
        }
    };

    let mut invocations = Vec::new();
    for backend in backends {
        let packages: Vec<RemoveTarget> = selected_indices
            .iter()
            .map(|index| &candidates[*index - 1])
            .filter(|candidate| candidate.backend == backend)
            .map(remove_target_from_candidate)
            .collect();
        if !packages.is_empty() {
            invocations.extend(backend.plan_remove(&packages, runtime));
        }
    }

    execute_invocations(invocations, dry_run, false, runtime)
}

fn run_interactive_upgrade(
    backends: Vec<Backend>,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let candidates = collect_upgradable_candidates(&backends, runtime)?;
    if candidates.is_empty() {
        println!("No upgradable packages found.");
        return Ok(ExitCode::SUCCESS);
    }

    print_candidates(&candidates);
    if dry_run {
        println!(
            "dry-run: upgradable-package scan executed, but upgrade commands will only be printed after selection."
        );
    }

    let selected_indices = prompt_for_selection("upgrade", candidates.len())?;
    if selected_indices.is_empty() {
        println!("No selection made. Nothing to upgrade.");
        return Ok(ExitCode::SUCCESS);
    }

    let mut invocations = Vec::new();
    for backend in backends {
        let packages: Vec<String> = selected_indices
            .iter()
            .map(|index| &candidates[*index - 1])
            .filter(|candidate| candidate.backend == backend)
            .map(|candidate| candidate.install_id.clone())
            .collect();
        if !packages.is_empty() {
            invocations.extend(backend.plan_upgrade(&packages, runtime));
        }
    }

    execute_invocations(invocations, dry_run, true, runtime)
}

fn run_update_all(
    backends: Vec<Backend>,
    dry_run: bool,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let total = backends.len();
    let mut failures = Vec::new();

    for backend in backends {
        if !dry_run {
            println!("{}", update_backend_summary(backend));
        }
        if let Err(error) =
            execute_invocations(backend.plan_update(runtime), dry_run, false, runtime)
        {
            eprintln!("warning: failed to update {backend}: {error}");
            failures.push(format!("{backend}: {error}"));
        }
    }

    if failures.is_empty() || failures.len() < total {
        Ok(ExitCode::SUCCESS)
    } else {
        Err(format!(
            "failed to update any backend: {}",
            failures.join("; ")
        ))
    }
}

fn run_search(
    backends: Vec<Backend>,
    query: &str,
    runtime: &RuntimeSettings<'_>,
    json: bool,
) -> Result<ExitCode, String> {
    let candidates = collect_search_candidates(&backends, query, runtime, json)?;
    if json {
        println!("{}", render_search_results_json(&candidates));
    } else if candidates.is_empty() {
        println!("No matching packages found.");
    } else {
        print_candidates(&candidates);
    }
    Ok(ExitCode::SUCCESS)
}

/// One entry in an exported manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestPackage {
    backend: Backend,
    id: String,
    name: String,
    version: Option<String>,
}

/// Packages winget can list but cannot reinstall: ARP and MSIX entries are
/// inventory resources, not installable identifiers. On this machine 111 of
/// 205 listed packages are like this, so exporting them would produce a
/// manifest that fails on import.
fn is_installable_package_id(backend: Backend, package_id: &str) -> bool {
    if backend != Backend::Winget {
        return true;
    }
    !(package_id.starts_with("ARP\\") || package_id.starts_with("MSIX\\"))
}

fn collect_manifest_packages(
    backends: &[Backend],
    runtime: &RuntimeSettings<'_>,
) -> Result<Vec<ManifestPackage>, String> {
    let mut packages = Vec::new();
    let aggregate = backends.len() > 1;
    let mut failures = Vec::new();

    for backend in backends {
        let invocation = backend
            .plan_list(false, runtime)
            .into_iter()
            .next()
            .expect("list should always produce one invocation");
        if aggregate {
            println!("Collecting installed packages from {backend}");
        }

        let capture = match run_capture_detailed_with_label_when(
            &invocation,
            &format!("Collecting {backend} packages"),
            false,
            runtime.command_timeout,
            |capture| backend.accepts_list_capture(false, capture),
        ) {
            Ok(capture) => capture,
            Err(error) => {
                if aggregate {
                    eprintln!("warning: failed to collect {backend}: {error}");
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error);
            }
        };
        if !backend.accepts_list_capture(false, &capture) {
            let error = render_command_failure(&invocation, &capture);
            if aggregate {
                eprintln!("warning: failed to collect {backend}: {error}");
                failures.push(format!("{backend}: {error}"));
                continue;
            }
            return Err(error);
        }

        let Some(rows) = backend.parse_list_entries(false, &capture.stdout) else {
            let error = format!("could not parse installed packages from {backend}");
            if aggregate {
                eprintln!("warning: {error}");
                failures.push(format!("{backend}: {error}"));
                continue;
            }
            return Err(error);
        };

        let skipped = rows
            .iter()
            .filter(|row| !is_installable_package_id(row.backend, &row.package_id))
            .count();
        if skipped > 0 {
            eprintln!(
                "note: skipping {skipped} {backend} package(s) that cannot be reinstalled by id"
            );
        }

        packages.extend(
            rows.into_iter()
                .filter(|row| is_installable_package_id(row.backend, &row.package_id))
                .map(|row| ManifestPackage {
                    backend: row.backend,
                    id: row.package_id,
                    name: row.name,
                    version: Some(row.current_version),
                }),
        );
    }

    if let Some(error) = all_backends_failed("export packages", backends.len(), &failures) {
        return Err(error);
    }

    Ok(packages)
}

fn render_manifest_json(packages: &[ManifestPackage]) -> String {
    let entries = packages
        .iter()
        .map(|package| {
            format!(
                "    {{\"backend\":\"{}\",\"id\":\"{}\",\"name\":\"{}\",\"version\":{}}}",
                package.backend,
                escape_json_string(&package.id),
                escape_json_string(&package.name),
                render_json_optional_string(package.version.as_deref()),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{{\n  \"schema\": \"waw-packages/1\",\n  \"packages\": [\n{entries}\n  ]\n}}\n"
    )
}

/// `waw bucket list` -- show the buckets Scoop actually has installed, plus
/// which ones this config prefers.
///
/// Delegates to `scoop bucket list` rather than reading Scoop's internal
/// state, so the answer always matches what Scoop itself would use.
fn run_bucket_command(
    action: BucketAction,
    config: &Config,
    dry_run: bool,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    match action {
        BucketAction::List => {
            // Resolve scoop the same way every other command does, so an
            // override or a non-PATH install still works.
            let invocation = Invocation::owned(
                &Backend::Scoop.preferred_program(),
                vec!["bucket".to_string(), "list".to_string()],
            );
            if dry_run {
                println!("> {}", invocation.render_for_display());
                return Ok(ExitCode::SUCCESS);
            }

            let capture = run_capture_detailed_with_label(
                &invocation,
                "Listing scoop buckets",
                runtime.command_timeout,
            )?;
            if !capture.success {
                return Err(render_command_failure(&invocation, &capture));
            }

            // `scoop bucket list` prints a "Name Source Updated Manifests"
            // table; keep only the bucket names.
            let installed: Vec<String> = capture
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .filter(|line| !line.starts_with("Name"))
                .filter(|line| !line.starts_with("----"))
                .filter_map(|line| line.split_whitespace().next())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect();

            println!("Scoop buckets:");
            for name in &installed {
                let preferred = config
                    .scoop_default_bucket
                    .as_deref()
                    .is_some_and(|default| default.eq_ignore_ascii_case(name));
                let listed = config
                    .scoop_buckets
                    .iter()
                    .any(|bucket| bucket.eq_ignore_ascii_case(name));
                let marker = match (preferred, listed) {
                    (true, _) => "  (default)",
                    (false, true) => "  (configured)",
                    (false, false) => "",
                };
                println!("  {name}{marker}");
            }
            if installed.is_empty() {
                println!("  (none reported by scoop)");
            }

            if let Some(default) = &config.scoop_default_bucket {
                println!();
                println!("Unqualified packages resolve to: {default}/<name>");
            }

            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_export(
    backends: Vec<Backend>,
    path: Option<PathBuf>,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let packages = collect_manifest_packages(&backends, runtime)?;
    let rendered = render_manifest_json(&packages);

    match path {
        Some(path) => {
            fs::write(&path, rendered)
                .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
            println!(
                "Exported {} package(s) to {}",
                packages.len(),
                path.display()
            );
        }
        None => print!("{rendered}"),
    }

    Ok(ExitCode::SUCCESS)
}

fn parse_manifest(contents: &str) -> Result<Vec<ManifestPackage>, String> {
    let root = parse_json(contents)?;

    // Accept our own manifest as well as winget's export format, so that
    // `winget export` output can be fed straight to `waw import`.
    if let Some(packages) = root.get("packages").and_then(JsonValue::as_array) {
        return packages
            .iter()
            .map(|entry| {
                let backend_name = entry
                    .get("backend")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("winget");
                let backend = Backend::parse(backend_name)?;
                let id = entry
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| "manifest package is missing \"id\"".to_string())?;
                Ok(ManifestPackage {
                    backend,
                    id: id.to_string(),
                    name: entry
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or(id)
                        .to_string(),
                    version: entry
                        .get("version")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string),
                })
            })
            .collect();
    }

    if let Some(sources) = root.get("Sources").and_then(JsonValue::as_array) {
        let mut packages = Vec::new();
        for source in sources {
            let Some(entries) = source.get("Packages").and_then(JsonValue::as_array) else {
                continue;
            };
            for entry in entries {
                if let Some(id) = entry.get("PackageIdentifier").and_then(JsonValue::as_str) {
                    packages.push(ManifestPackage {
                        backend: Backend::Winget,
                        id: id.to_string(),
                        name: id.to_string(),
                        version: None,
                    });
                }
            }
        }
        if !packages.is_empty() {
            return Ok(packages);
        }
    }

    Err("manifest is neither a waw manifest nor a winget export file".to_string())
}

fn run_import(
    path: &Path,
    dry_run: bool,
    selected_backend: Option<Backend>,
    config: &Config,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let packages = parse_manifest(&contents)?;
    if packages.is_empty() {
        println!("Manifest contains no packages.");
        return Ok(ExitCode::SUCCESS);
    }

    let mut groups: Vec<(Backend, Vec<String>)> = Vec::new();
    let mut skipped = 0usize;
    for package in &packages {
        if let Some(selected) = selected_backend {
            if selected != package.backend {
                skipped += 1;
                continue;
            }
        } else if !config.is_backend_enabled(package.backend) {
            skipped += 1;
            continue;
        }
        match groups.iter_mut().find(|(backend, _)| *backend == package.backend) {
            Some((_, ids)) => ids.push(package.id.clone()),
            None => groups.push((package.backend, vec![package.id.clone()])),
        }
    }

    if skipped > 0 {
        eprintln!("note: skipping {skipped} package(s) for backends that are disabled or filtered out");
    }
    if groups.is_empty() {
        println!("Nothing to install from this manifest.");
        return Ok(ExitCode::SUCCESS);
    }

    let mut invocations = Vec::new();
    for (backend, ids) in &groups {
        println!(
            "Importing {} package(s) with {backend}",
            ids.len()
        );
        let specs: Vec<PackageSpec> = ids
            .iter()
            .map(|id| PackageSpec::without_version(id))
            .collect();
        invocations.extend(backend.plan_install(&specs, runtime));
    }

    execute_invocations(invocations, dry_run, true, runtime)
}

fn run_list(
    backends: Vec<Backend>,
    upgradable: bool,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
    json: bool,
) -> Result<ExitCode, String> {
    let single_backend = backends.len() == 1;
    let noun = if upgradable {
        "upgradable packages"
    } else {
        "installed packages"
    };
    let mut parsed_rows = Vec::new();
    let mut raw_sections = Vec::new();

    if !json {
        for backend in &backends {
            println!("Listing {noun} from {backend}");
        }
    }

    let invocations: Vec<Invocation> = backends
        .iter()
        .map(|backend| {
            backend
                .plan_list(upgradable, runtime)
                .into_iter()
                .next()
                .expect("list should always produce one invocation")
        })
        .collect();

    if dry_run {
        for invocation in &invocations {
            println!("> {}", invocation.render_for_display());
        }
        return Ok(ExitCode::SUCCESS);
    }

    let labels: Vec<String> = backends
        .iter()
        .map(|backend| format!("Listing {noun} from {backend}"))
        .collect();
    let captures = if single_backend {
        invocations
            .iter()
            .zip(labels)
            .map(|(invocation, label)| {
                run_capture_detailed_with_label(invocation, &label, runtime.command_timeout)
            })
            .collect()
    } else {
        run_captures_parallel(invocations.clone(), labels, runtime.command_timeout)
    };

    let mut failures = Vec::new();
    for (index, backend) in backends.iter().enumerate() {
        let capture = match &captures[index] {
            Ok(capture) => capture,
            Err(error) => {
                if single_backend {
                    return Err(error.clone());
                }
                eprintln!("warning: failed to list {noun} from {backend}: {error}");
                failures.push(format!("{backend}: {error}"));
                continue;
            }
        };
        if !backend.accepts_list_capture(upgradable, capture) {
            let error = render_command_failure(&invocations[index], capture);
            if single_backend {
                return Err(error);
            }
            eprintln!("warning: failed to list {noun} from {backend}: {error}");
            failures.push(format!("{backend}: {error}"));
            continue;
        }

        if let Some(rows) = backend.parse_list_entries(upgradable, &capture.stdout) {
            parsed_rows.extend(rows);
        } else if let Some(section) = render_backend_output_section(*backend, &capture.stdout) {
            raw_sections.push(section);
        }
    }

    if let Some(error) = all_backends_failed(noun, backends.len(), &failures) {
        return Err(error);
    }

    let parsed_rows = sort_list_rows(parsed_rows);
    if json {
        println!("{}", render_list_results_json(&parsed_rows, upgradable));
        return Ok(ExitCode::SUCCESS);
    }
    if !parsed_rows.is_empty() {
        print_list_rows(&parsed_rows, upgradable);
    }

    if !raw_sections.is_empty() {
        if !parsed_rows.is_empty() {
            println!();
            println!("Unparsed backend output:");
        }
        for section in &raw_sections {
            println!();
            println!("{section}");
        }
    }

    if !parsed_rows.is_empty() || !raw_sections.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        println!("No {noun} found.");
        Ok(ExitCode::SUCCESS)
    }
}

fn run_show(
    backends: Vec<Backend>,
    package: &str,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
    json: bool,
) -> Result<ExitCode, String> {
    let single_backend = backends.len() == 1;
    let mut parsed_details = Vec::new();
    let mut raw_sections = Vec::new();
    let mut json_results = Vec::new();

    for backend in backends {
        let invocation = backend
            .plan_show(package, runtime)
            .into_iter()
            .next()
            .expect("show should always produce one invocation");
        if !json {
            println!("Showing package details from {backend}: {package}");
        }

        if dry_run {
            if json {
                json_results.push(ShowBackendResult {
                    backend,
                    command: invocation.render_for_display(),
                    success: true,
                    dry_run: true,
                    details: None,
                    raw_output: None,
                    error: None,
                });
            }
            continue;
        }

        let capture = match run_capture_detailed_with_label(
            &invocation,
            &format!("Showing {package} from {backend}"),
            runtime.command_timeout,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                if json {
                    json_results.push(ShowBackendResult {
                        backend,
                        command: invocation.render_for_display(),
                        success: false,
                        dry_run: false,
                        details: None,
                        raw_output: None,
                        error: Some(error.clone()),
                    });
                }
                if single_backend && !json {
                    return Err(error);
                }
                continue;
            }
        };
        if !capture.success {
            let error = render_command_failure(&invocation, &capture);
            if json {
                json_results.push(ShowBackendResult {
                    backend,
                    command: invocation.render_for_display(),
                    success: false,
                    dry_run: false,
                    details: None,
                    raw_output: None,
                    error: Some(error.clone()),
                });
            }
            if single_backend && !json {
                return Err(error);
            }
            continue;
        }

        if let Some(details) = backend.parse_show_details(&capture.stdout) {
            if json {
                json_results.push(ShowBackendResult {
                    backend,
                    command: invocation.render_for_display(),
                    success: true,
                    dry_run: false,
                    details: Some(details.clone()),
                    raw_output: None,
                    error: None,
                });
            }
            parsed_details.push(details);
        } else if let Some(section) = render_backend_output_section(backend, &capture.stdout) {
            if json {
                json_results.push(ShowBackendResult {
                    backend,
                    command: invocation.render_for_display(),
                    success: true,
                    dry_run: false,
                    details: None,
                    raw_output: Some(capture.stdout.trim().to_string()),
                    error: None,
                });
            }
            raw_sections.push(section);
        }
    }

    if json {
        println!("{}", render_show_results_json(&json_results));
        return Ok(ExitCode::SUCCESS);
    }

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    if !parsed_details.is_empty() {
        print_package_details_sections(&parsed_details);
    }

    if !raw_sections.is_empty() {
        if !parsed_details.is_empty() {
            println!();
            println!("Unparsed backend output:");
        }
        for section in &raw_sections {
            println!();
            println!("{section}");
        }
    }

    if !parsed_details.is_empty() || !raw_sections.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Err(format!(
            "no package details were found for `{package}` in enabled backends"
        ))
    }
}

fn run_captures_parallel(
    invocations: Vec<Invocation>,
    labels: Vec<String>,
    timeout: Option<Duration>,
) -> Vec<Result<CommandCapture, String>> {
    let mut handles = Vec::with_capacity(invocations.len());
    for (invocation, label) in invocations.into_iter().zip(labels) {
        let handle = thread::spawn(move || {
            let _ = label;
            run_quiet_command_capture(&invocation, timeout)
        });
        handles.push(handle);
    }
    handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| Err("backend thread panicked".to_string()))
        })
        .collect()
}

fn collect_search_candidates(
    backends: &[Backend],
    query: &str,
    runtime: &RuntimeSettings<'_>,
    quiet: bool,
) -> Result<Vec<SearchCandidate>, String> {
    let aggregate = backends.len() > 1;

    for backend in backends {
        if !quiet {
            if aggregate {
                println!("Searching {backend} for: {query}");
            } else {
                println!("Searching candidates for: {query}");
            }
        }
    }

    let invocations: Vec<Invocation> = backends
        .iter()
        .map(|backend| backend.search_invocation(query, runtime))
        .collect();
    let labels: Vec<String> = backends
        .iter()
        .map(|backend| format!("Searching {backend} for {query}"))
        .collect();

    let captures = if aggregate && !quiet {
        run_captures_parallel(invocations.clone(), labels, runtime.command_timeout)
    } else {
        invocations
            .iter()
            .zip(labels)
            .map(|(invocation, label)| {
                run_capture_detailed_with_label(invocation, &label, runtime.command_timeout)
            })
            .collect()
    };

    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for (index, backend) in backends.iter().enumerate() {
        match &captures[index] {
            Ok(capture) if capture.success => {
                candidates.extend(backend.parse_search_candidates(&capture.stdout));
            }
            Ok(capture) => {
                let error = render_command_failure(&invocations[index], capture);
                if aggregate {
                    eprintln!("warning: failed to search {backend}: {error}");
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error);
            }
            Err(error) => {
                if aggregate {
                    eprintln!("warning: failed to search {backend}: {error}");
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error.clone());
            }
        }
    }

    if let Some(error) = all_backends_failed("search", backends.len(), &failures) {
        return Err(error);
    }

    let candidates = dedupe_search_candidates(candidates);
    Ok(sort_search_candidates(candidates, query, backends))
}

fn collect_installed_candidates(
    backends: &[Backend],
    query: &str,
    runtime: &RuntimeSettings<'_>,
) -> Result<Vec<SearchCandidate>, String> {
    let mut candidates = Vec::new();
    let aggregate = backends.len() > 1;
    let normalized_query = normalize_search_text(query);
    let mut failures = Vec::new();

    for backend in backends {
        let invocation = backend
            .plan_list(false, runtime)
            .into_iter()
            .next()
            .expect("installed package listing should always produce one invocation");
        if aggregate {
            println!("Searching installed packages in {backend} for: {query}");
        } else {
            println!("Searching installed packages for: {query}");
        }

        let capture = match run_capture_detailed_with_label(
            &invocation,
            &format!("Searching installed packages in {backend} for {query}"),
            runtime.command_timeout,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                if aggregate {
                    eprintln!(
                        "warning: failed to inspect installed packages in {backend}: {error}"
                    );
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error);
            }
        };
        if !backend.accepts_list_capture(false, &capture) {
            let error = render_command_failure(&invocation, &capture);
            if aggregate {
                eprintln!("warning: failed to inspect installed packages in {backend}: {error}");
                failures.push(format!("{backend}: {error}"));
                continue;
            }
            return Err(error);
        }

        let Some(rows) = backend.parse_list_entries(false, &capture.stdout) else {
            let error = format!(
                "installed package output from {backend} could not be parsed for selection"
            );
            if aggregate {
                eprintln!("warning: {error}");
                continue;
            }
            return Err(error);
        };

        candidates.extend(rows.into_iter().filter_map(|row| {
            package_list_entry_matches_query(&row, &normalized_query)
                .then(|| package_list_entry_candidate(row))
        }));
    }

    if let Some(error) = all_backends_failed("inspect installed packages", backends.len(), &failures)
    {
        return Err(error);
    }

    let candidates = dedupe_installed_candidates(candidates);
    let candidates = sort_search_candidates(candidates, query, backends);
    Ok(candidates)
}

fn collect_upgradable_candidates(
    backends: &[Backend],
    runtime: &RuntimeSettings<'_>,
) -> Result<Vec<SearchCandidate>, String> {
    let aggregate = backends.len() > 1;
    let invocations: Vec<Invocation> = backends
        .iter()
        .map(|backend| {
            backend
                .plan_list(true, runtime)
                .into_iter()
                .next()
                .expect("upgradable list should always produce one invocation")
        })
        .collect();
    let labels: Vec<String> = backends
        .iter()
        .map(|backend| format!("Checking updates for {backend}"))
        .collect();

    let captures = if aggregate {
        run_captures_parallel(invocations.clone(), labels, runtime.command_timeout)
    } else {
        invocations
            .iter()
            .zip(labels)
            .map(|(invocation, label)| {
                run_capture_detailed_with_label(invocation, &label, runtime.command_timeout)
            })
            .collect()
    };

    let mut candidates = Vec::new();
    let mut failures = Vec::new();
    for (index, backend) in backends.iter().enumerate() {
        let capture = match &captures[index] {
            Ok(capture) => capture,
            Err(error) => {
                if aggregate {
                    eprintln!(
                        "warning: failed to inspect pending upgrades for {backend}: {error}"
                    );
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error.clone());
            }
        };
        if !backend.accepts_list_capture(true, capture) {
            let error = render_command_failure(&invocations[index], capture);
            if aggregate {
                eprintln!(
                    "warning: failed to inspect pending upgrades for {backend}: {error}"
                );
                failures.push(format!("{backend}: {error}"));
                continue;
            }
            return Err(error);
        }
        let rows = backend
            .parse_list_entries(true, &capture.stdout)
            .unwrap_or_default();
        candidates.extend(rows.into_iter().map(package_upgrade_candidate));
    }

    if let Some(error) =
        all_backends_failed("inspect pending upgrades", backends.len(), &failures)
    {
        return Err(error);
    }

    let candidates = dedupe_installed_candidates(candidates);
    Ok(sort_search_candidates(candidates, "", backends))
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

fn print_candidates(candidates: &[SearchCandidate]) {
    println!();
    for (index, candidate) in candidates.iter().enumerate() {
        let version = candidate.version.as_deref().unwrap_or("unknown");
        let backend_label = candidate.backend.to_string();
        let backend_details = match candidate.source.as_deref() {
            Some(source) if !source.eq_ignore_ascii_case(&backend_label) => {
                format!("{backend_label}, {source}")
            }
            _ => backend_label,
        };
        println!(
            "{:>3}) {} [{}]  {}  ({})",
            index + 1,
            candidate.label,
            candidate.install_id,
            version,
            backend_details
        );
    }
    println!();
}

fn choose_candidates(
    action: &str,
    candidates: &[SearchCandidate],
    assume_yes: bool,
) -> Result<Option<Vec<usize>>, String> {
    // Interactive terminal keeps the original behaviour: always prompt.
    if interactive_stdin_available() {
        return prompt_for_selection(action, candidates.len()).map(Some);
    }

    // Non-interactive: never block on stdin.
    if assume_yes {
        let exact: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate.label.eq_ignore_ascii_case(candidate.install_id.as_str())
            })
            .map(|(index, _)| index + 1)
            .collect();
        if candidates.len() == 1 {
            println!(
                "auto-selected {} (non-interactive; only one candidate)",
                candidates[0].install_id
            );
            return Ok(Some(vec![1]));
        }
        if exact.len() == 1 {
            println!(
                "auto-selected {} (non-interactive; unique exact match)",
                candidates[exact[0] - 1].install_id
            );
            return Ok(Some(exact));
        }
    }

    Err(format!(
        "`{action}` found {} candidates but stdin is not a terminal. \
         Use `{action} --exact <package>` to choose one explicitly.",
        candidates.len()
    ))
}

fn interactive_stdin_available() -> bool {
    io::stdin().is_terminal()
        || env::var("WAW_FORCE_INTERACTIVE")
            .map(|value| !value.trim().is_empty() && value.trim() != "0")
            .unwrap_or(false)
}

fn prompt_for_selection(action: &str, max: usize) -> Result<Vec<usize>, String> {
    if !interactive_stdin_available() {
        return Err(format!(
            "interactive selection for `{action}` requires a terminal. \
             Re-run with `--exact` to name packages directly, or from an interactive shell."
        ));
    }
    print!("Select packages to {action} (e.g. 1 2 5, 1,3-4; empty to cancel): ");
    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush stdout: {error}"))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| format!("failed to read selection: {error}"))?;

    parse_selection(&input, max)
}

fn parse_selection(input: &str, max: usize) -> Result<Vec<usize>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut selected = Vec::new();
    for token in trimmed.replace(',', " ").split_whitespace() {
        if let Some((start_raw, end_raw)) = token.split_once('-') {
            let start = parse_index(start_raw, max)?;
            let end = parse_index(end_raw, max)?;
            if start > end {
                return Err(format!("invalid range {token}: start must be <= end"));
            }
            for value in start..=end {
                if !selected.contains(&value) {
                    selected.push(value);
                }
            }
        } else {
            let value = parse_index(token, max)?;
            if !selected.contains(&value) {
                selected.push(value);
            }
        }
    }

    Ok(selected)
}

fn parse_index(raw: &str, max: usize) -> Result<usize, String> {
    let value = raw
        .parse::<usize>()
        .map_err(|_| format!("invalid selection value: {raw}"))?;
    if value == 0 || value > max {
        Err(format!(
            "selection out of range: {raw} (expected 1..={max})"
        ))
    } else {
        Ok(value)
    }
}

fn env_override(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn command_works_or_is_discoverable(program: &str, args: &[String]) -> bool {
    command_works(program, args) || path_looks_present(Path::new(program))
}

fn path_looks_present(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => true,
        Err(_) => false,
    }
}

fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

fn push_unique_path_if_present(items: &mut Vec<String>, path: PathBuf) {
    if path_looks_present(&path) {
        push_unique_string(items, path.display().to_string());
    }
}

fn find_programs_on_path(path_var: Option<&OsStr>, names: &[&str]) -> Vec<String> {
    let mut programs = Vec::new();
    let Some(path_var) = path_var else {
        return programs;
    };

    for dir in env::split_paths(path_var) {
        for name in names {
            let candidate = dir.join(name);
            if path_looks_present(&candidate) {
                push_unique_string(&mut programs, candidate.display().to_string());
            }
        }
    }

    programs
}

fn user_home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(PathBuf::from))
}

fn local_app_data_dir() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join("AppData").join("Local")))
}

fn program_data_dir() -> PathBuf {
    env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

fn winget_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = local_app_data_dir() {
        paths.push(
            local_app_data
                .join("Microsoft")
                .join("WindowsApps")
                .join("winget.exe"),
        );
    }
    paths
}

fn scoop_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(scoop_root) = env::var_os("SCOOP")
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join("scoop")))
    {
        paths.push(scoop_root.join("shims").join("scoop.cmd"));
        paths.push(scoop_root.join("shims").join("scoop.ps1"));
    }
    paths
}

fn choco_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(choco_root) = env::var_os("ChocolateyInstall").map(PathBuf::from) {
        paths.push(choco_root.join("bin").join("choco.exe"));
    }
    if let Some(local_app_data) = local_app_data_dir() {
        paths.push(
            local_app_data
                .join("UniGetUI")
                .join("Chocolatey")
                .join("bin")
                .join("choco.exe"),
        );
    }
    paths.push(
        program_data_dir()
            .join("chocolatey")
            .join("bin")
            .join("choco.exe"),
    );
    paths
}

fn npm_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if cfg!(windows) {
        if let Some(program_files) = env::var_os("ProgramFiles").map(PathBuf::from) {
            paths.push(program_files.join("nodejs").join("npm.cmd"));
        }
        if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)").map(PathBuf::from) {
            paths.push(program_files_x86.join("nodejs").join("npm.cmd"));
        }
    }
    paths
}

fn pip_known_program_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = local_app_data_dir() {
        let python_root = local_app_data.join("Programs").join("Python");
        paths.push(python_root.join("Launcher").join("py.exe"));

        if let Ok(entries) = fs::read_dir(&python_root) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("python") {
                    let base = entry.path();
                    paths.push(base.join("python.exe"));
                    paths.push(base.join("Scripts").join("pip.exe"));
                }
            }
        }
    }
    paths
}

fn pip_path_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["python.exe", "python3.exe", "py.exe", "pip.exe", "pip3.exe"]
    } else {
        &["python", "python3", "pip", "pip3"]
    }
}

fn is_python_launcher(program: &str) -> bool {
    let lower = program.to_ascii_lowercase();
    lower.ends_with("python")
        || lower.ends_with("python.exe")
        || lower.ends_with("python3")
        || lower.ends_with("python3.exe")
        || lower.ends_with("\\py.exe")
        || lower == "py"
        || lower == "py.exe"
}

fn pip_probe_args(program: &str) -> Vec<String> {
    if is_python_launcher(program) {
        vec!["-m".to_string(), "pip".to_string(), "--version".to_string()]
    } else {
        vec!["--version".to_string()]
    }
}

impl Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Winget => "winget",
            Self::Scoop => "scoop",
            Self::Chocolatey => "choco",
            Self::Npm => "npm",
            Self::Pip => "pip",
        })
    }
}

/// PyPI has no search API: the XML-RPC `search` endpoint was permanently
/// disabled in 2020 because it was hammered into the ground, and the web UI's
/// `/search/` route is not a machine API.
///
/// The previous implementation worked around that by downloading the entire
/// simple index and filtering locally. Measured on 2026-10-27 that was
/// 45,363,902 bytes, 924,326 projects, and roughly 82 seconds per query with
/// no User-Agent, no compression, and no timeout.
///
/// This probes the documented per-project JSON API instead. It only finds
/// projects whose name it can guess from the query, which is stated in the
/// CLI help. In exchange a search costs a handful of small requests.
/// PyPI has no search API: the XML-RPC `search` endpoint was permanently
/// disabled in 2020, and the web UI's `/search/` route is not a machine API.
///
/// This probes the documented per-project JSON API with names derived from the
/// query. It costs a handful of small requests instead of the 45 MB / ~82 s
/// full-index download the previous implementation did.
///
/// Two refinements over naive probing:
///
/// * Version-suffixed names are probed, because several widely used projects
///   were renamed that way (`beautifulsoup` -> `beautifulsoup4`). Without this
///   a query for "beautifulsoup" returns the abandoned 2008 package.
/// * Yanked releases are ranked last, and a package with a strictly newer
///   release wins a tie against a same-named older sibling.
const PIP_SEARCH_SCRIPT: &str = r#"import gzip, json, os, sys, time, urllib.error, urllib.request
query = sys.argv[1].strip()
normalized = query.lower().replace(" ", "-").replace("_", "-")

candidates = []
def add(name):
    lowered = name.lower()
    if lowered and lowered not in candidates:
        candidates.append(lowered)

add(normalized)
# Renamed projects: beautifulsoup -> beautifulsoup4, pymysql -> PyMySQL, etc.
for suffix in ("4", "3", "2"):
    add(normalized + suffix)
for prefix in ("python-", "py-", "django-", "flask-"):
    add(prefix + normalized)
add(normalized + "-cli")
add(normalized + "-api")
add(normalized + "-sdk")
add(normalized + "-tools")
add(normalized + "-client")
add(normalized + "-server")
add(normalized + "-utils")
add(normalized + "-parser")
add(normalized + "-py")
parts = normalized.split("-")
if len(parts) > 1:
    add(parts[0])
    add("".join(parts))
    add("-".join(reversed(parts)))

def fetch(name):
    url = "https://pypi.org/pypi/" + name + "/json"
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": "waw-cli"},
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            data = json.load(response)
    except urllib.error.HTTPError:
        return None
    except Exception:
        return None
    info = data.get("info") or {}
    real_name = info.get("name")
    if not real_name:
        return None

    # Newest upload time across all releases, used to spot abandoned packages.
    newest = ""
    yanked = 0
    total = 0
    for files in (data.get("releases") or {}).values():
        for entry in files:
            total += 1
            if entry.get("yanked"):
                yanked += 1
            uploaded = entry.get("upload_time_iso_8601") or ""
            if uploaded > newest:
                newest = uploaded

    return {
        "name": real_name,
        "version": info.get("version"),
        "summary": info.get("summary"),
        "newest": newest,
        "fully_yanked": total > 0 and yanked == total,
    }

def rank_key(item):
    name = item["name"].lower()
    # Names that are the query or a numeric/word variant of it form one class;
    # among them, recency decides. This is what puts beautifulsoup4 ahead of
    # the abandoned beautifulsoup without demoting an exact match that is
    # itself current.
    same_family = name == normalized or (
        name.startswith(normalized)
        and name[len(normalized):len(normalized) + 1].isdigit()
    )
    # Invert the timestamp so "newer" sorts first.
    inverted_date = "".join(
        chr(255 - ord(c)) if ord(c) < 255 else c for c in item["newest"]
    )
    return (
        # Yanked packages never lead.
        item["fully_yanked"],
        not same_family,
        not name.startswith(normalized),
        inverted_date,
        name != normalized,
        len(name),
        name,
    )

if len(sys.argv) > 2 and sys.argv[2] == "--deep":
    cache_path = sys.argv[3] if len(sys.argv) > 3 else ""
    index_url = "https://pypi.org/simple/"
    index_headers = {
        "Accept": "application/vnd.pypi.simple.v1+json",
        "User-Agent": "waw-cli",
        "Accept-Encoding": "gzip",
    }

    cache = {}
    if cache_path:
        try:
            with open(cache_path, "r", encoding="utf-8") as handle:
                cache = json.load(handle)
        except Exception:
            cache = {}

    names = cache.get("names") or []
    etag = cache.get("etag")
    fetched_at = cache.get("fetched_at") or 0
    # Respect Cache-Control: max-age=600. Revalidating on every query would add
    # a network round trip for a list that cannot have changed yet.
    stale = (time.time() - fetched_at) > 600

    if not names or stale:
        request_headers = dict(index_headers)
        if etag:
            request_headers["If-None-Match"] = etag
        try:
            request = urllib.request.Request(index_url, headers=request_headers)
            with urllib.request.urlopen(request, timeout=60) as response:
                body = response.read()
                if response.headers.get("Content-Encoding") == "gzip":
                    body = gzip.decompress(body)
                payload = json.loads(body)
                names = [
                    project.get("name")
                    for project in payload.get("projects", [])
                    if isinstance(project, dict) and project.get("name")
                ]
                etag = response.headers.get("ETag") or etag
                stale = False
        except urllib.error.HTTPError as error:
            # 304 means the cached list is still current.
            if error.code != 304:
                names = []
        except Exception:
            # Network failure falls back to whatever cache exists.
            pass

        if cache_path and names:
            try:
                os.makedirs(os.path.dirname(cache_path), exist_ok=True)
                with open(cache_path, "w", encoding="utf-8") as handle:
                    json.dump(
                        {"names": names, "etag": etag, "fetched_at": time.time()},
                        handle,
                    )
            except Exception:
                pass

    needle = normalized
    matches = [name for name in names if needle in name.lower()]
    # Prefer names that start with the query, then shorter names, then
    # alphabetical, and cap the detail lookups.
    matches.sort(
        key=lambda name: (
            not name.lower().startswith(needle),
            len(name),
            name.lower(),
        )
    )
    deep_hits = []
    for name in matches[:20]:
        info = fetch(name)
        if info:
            deep_hits.append(info)
    deep_hits.sort(key=rank_key)
    print(json.dumps(deep_hits))
    sys.exit(0)

hits = []
for candidate in candidates:
    info = fetch(candidate)
    if info:
        hits.append(info)

hits.sort(key=rank_key)

# --- --deep: search the full PyPI project index ----------------------------
#
# Probing only finds names the query can predict. With --deep, fetch the
# documented simple index instead and substring-match it.
#
# Measured 2026-10-28: 10 MB gzipped / 45 MB decoded / 925k projects / 2.4 s
# first fetch. The response carries an ETag and max-age=600, so later runs are
# a conditional GET returning 304 in ~0.4 s with no body.

print(json.dumps(hits))
"#;

fn append_source_arg(args: &mut Vec<String>, source: Option<&str>) {
    if let Some(source) = source {
        args.push("--source".to_string());
        args.push(source.to_string());
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
