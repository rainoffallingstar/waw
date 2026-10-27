mod backend;
mod capture;
mod elevation;
mod json;
mod parse;
#[cfg(windows)]
mod win32;

use json::{JsonValue, escape_json_string, parse_json};
use backend::Backend;
#[allow(unused_imports)]
use elevation::*;
#[allow(unused_imports)]
use capture::{decode_capture_text, read_capture_log, unique_capture_base};
use parse::*;
#[cfg(windows)]
use win32::{
    CloseHandle, CREATE_SUSPENDED, ERROR_CANCELLED, GetExitCodeProcess, Handle, OpenProcess,
    PROCESS_SET_QUOTA, PROCESS_TERMINATE, ProcessJob, SEE_MASK_NOCLOSEPROCESS, ShellExecuteExW,
    ShellExecuteInfoW, TerminateProcess, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    WaitForSingleObject, resume_process_threads,
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

    if let Subcommand::Export { path } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_export(backends, path.clone(), &runtime);
    }

    if let Subcommand::Import { path } = &cli.command {
        return run_import(path, cli.dry_run, selected_backend, config, &runtime);
    }

    if let Subcommand::Search { query } = &cli.command {
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

fn execute_invocations(
    invocations: Vec<Invocation>,
    dry_run: bool,
    continue_on_error: bool,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let process_is_elevated = runtime.auto_elevate && is_process_elevated();
    let should_batch_elevate = cfg!(windows)
        && runtime.auto_elevate
        && !process_is_elevated
        && invocations
            .iter()
            .any(|invocation| invocation.requires_elevation && !invocation.program.is_empty());

    for invocation in &invocations {
        if let Some(message) = invocation.message.as_deref() {
            println!("{message}");
        }

        if invocation.program.is_empty() {
            continue;
        }

        if dry_run {
            println!("> {}", invocation.render_for_display());
            continue;
        }
    }

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    if should_batch_elevate {
        run_elevated_invocations(&invocations, continue_on_error, runtime.command_timeout)?;
        return Ok(ExitCode::SUCCESS);
    }

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();

    for invocation in invocations {
        if invocation.program.is_empty() {
            continue;
        }

        let display_command = invocation.render_for_display();
        let progress_label = invocation_progress_label(&invocation);
        let capture = if cfg!(windows)
            && runtime.auto_elevate
            && invocation.requires_elevation
            && !process_is_elevated
        {
            println!("requesting administrator privileges...");
            run_elevated_invocation(&invocation, &progress_label, runtime.command_timeout)?
        } else {
            run_logged_command_capture_with_label(
                &invocation,
                &progress_label,
                true,
                runtime.command_timeout,
                |_| false,
            )?
        };

        if !capture.success {
            if is_tolerable_command_failure(&invocation, &capture) {
                if continue_on_error {
                    succeeded.push(progress_label);
                }
                continue;
            }
            emit_command_logs(&display_command, &capture.stdout, &capture.stderr);
            let error = render_command_failure(&invocation, &capture);
            if continue_on_error {
                failed.push((progress_label, error));
                continue;
            }
            return Err(error);
        }

        if continue_on_error {
            succeeded.push(progress_label);
        }
    }

    if continue_on_error && !failed.is_empty() {
        if let Some(summary) = render_invocation_batch_summary(&succeeded, &failed) {
            eprintln!("{summary}");
        }
        return Err(format!(
            "{} command(s) failed during execution",
            failed.len()
        ));
    }

    Ok(ExitCode::SUCCESS)
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


fn run_capture_detailed_with_label(
    invocation: &Invocation,
    label: &str,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    run_capture_detailed_with_label_when(invocation, label, false, timeout, |_| false)
}

fn run_capture_detailed_with_label_when<F>(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: F,
) -> Result<CommandCapture, String>
where
    F: FnOnce(&CommandCapture) -> bool,
{
    run_logged_command_capture_with_label(invocation, label, inherit_stdin, timeout, accept_capture)
}

fn render_command_failure(invocation: &Invocation, capture: &CommandCapture) -> String {
    if capture.timed_out {
        return format!(
            "backend command timed out after {}s: {}",
            capture.timeout_seconds.unwrap_or(0),
            invocation.render_for_display()
        );
    }
    let stderr = capture.stderr.trim();
    if stderr.is_empty() {
        format!(
            "backend command failed with exit code {}: {}",
            capture.status_code,
            invocation.render_for_display()
        )
    } else {
        format!(
            "backend command failed with exit code {}: {} ({stderr})",
            capture.status_code,
            invocation.render_for_display()
        )
    }
}

fn render_backend_output_section(backend: Backend, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("== {backend} ==\n{trimmed}"))
    }
}

fn run_logged_command_capture_with_label(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: impl FnOnce(&CommandCapture) -> bool,
) -> Result<CommandCapture, String> {
    run_logged_command_capture_inner(
        invocation,
        label,
        inherit_stdin,
        timeout,
        accept_capture,
        false,
    )
}

fn run_quiet_command_capture(
    invocation: &Invocation,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    run_logged_command_capture_inner(invocation, "", false, timeout, |_| false, true)
}

/// Terminates a timed-out child and everything it spawned.
///
/// On Windows the child is attached to a job object at spawn time, so the whole
/// tree can be torn down in one call. `Child::kill` alone only reaches the
/// wrapper (`scoop.cmd`, `npm.cmd`, ...) and leaves grandchildren running.
fn kill_process_tree(child: &mut std::process::Child, job: Option<&ProcessJobHandle>) {
    #[cfg(windows)]
    if let Some(job) = job {
        job.terminate(1);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
type ProcessJobHandle = ProcessJob;
#[cfg(not(windows))]
type ProcessJobHandle = ();

fn run_logged_command_capture_inner(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: impl FnOnce(&CommandCapture) -> bool,
    quiet: bool,
) -> Result<CommandCapture, String> {
    let capture = CommandLogCapture::new()?;
    let stdout_file = fs::File::create(&capture.stdout_path)
        .map_err(|error| format!("failed to open stdout capture file: {error}"))?;
    let stderr_file = fs::File::create(&capture.stderr_path)
        .map_err(|error| format!("failed to open stderr capture file: {error}"))?;
    let progress_label = compact_progress_label(label);

    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(if inherit_stdin {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    // On Windows the child is started suspended so that it can be attached to
    // a job object before any of its threads run. That way a timed-out backend
    // takes down the wrapper script's grandchildren too, not just the wrapper.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {}: {error}", invocation.program))?;

    #[cfg(windows)]
    let job = {
        let job = ProcessJob::create();
        let pid = child.id();
        if let Some(active_job) = job.as_ref() {
            let process_handle = unsafe {
                OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid)
            };
            if !process_handle.is_null() {
                let assigned = active_job.assign(process_handle);
                unsafe {
                    CloseHandle(process_handle);
                }
                if !assigned {
                    // Assignment can fail when the process already belongs to a
                    // job with a conflicting limit; fall back to a plain kill.
                }
            }
        }
        resume_process_threads(pid);
        job
    };

    #[cfg(not(windows))]
    let job: Option<()> = None;

    let mut reporter = ProgressReporter::new_with_enabled(!quiet);
    let started_at = Instant::now();
    let mut timed_out = false;

    loop {
        reporter.tick(&progress_label)?;
        if child
            .try_wait()
            .map_err(|error| format!("failed to monitor {}: {error}", invocation.program))?
            .is_some()
        {
            break;
        }
        if timeout.is_some_and(|limit| started_at.elapsed() >= limit) {
            kill_process_tree(&mut child, job.as_ref());
            timed_out = true;
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }

    let status = if timed_out {
        None
    } else {
        Some(
            child
                .wait()
                .map_err(|error| format!("failed to collect {} result: {error}", invocation.program))?,
        )
    };

    let stdout = read_capture_log(&capture.stdout_path);
    let stderr = read_capture_log(&capture.stderr_path);

    let command_capture = CommandCapture {
        stdout,
        stderr,
        success: status.as_ref().is_some_and(|status| status.success()),
        status_code: status.as_ref().and_then(|status| status.code()).unwrap_or(1),
        timed_out,
        timeout_seconds: timeout.map(|limit| limit.as_secs()),
    };
    reporter.finish(
        command_capture.success || accept_capture(&command_capture),
        &progress_label,
    )?;

    Ok(command_capture)
}

fn render_invocation_batch_summary(
    succeeded: &[String],
    failed: &[(String, String)],
) -> Option<String> {
    if succeeded.is_empty() && failed.is_empty() {
        return None;
    }

    let mut lines = vec!["Batch summary:".to_string()];
    if !succeeded.is_empty() {
        lines.push(format!("Succeeded ({}):", succeeded.len()));
        lines.extend(succeeded.iter().map(|label| format!("- {label}")));
    }
    if !failed.is_empty() {
        lines.push(format!("Failed ({}):", failed.len()));
        lines.extend(
            failed
                .iter()
                .map(|(label, reason)| format!("- {label}: {reason}")),
        );
    }

    Some(lines.join("\n"))
}

fn emit_command_logs(command: &str, stdout: &str, stderr: &str) {
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();

    if stdout.is_empty() && stderr.is_empty() {
        return;
    }

    eprintln!("--- logs for `{command}` ---");
    if !stdout.is_empty() {
        eprintln!("[stdout]");
        eprintln!("{stdout}");
    }
    if !stderr.is_empty() {
        eprintln!("[stderr]");
        eprintln!("{stderr}");
    }
}

fn emit_successful_command_output(stdout: &str, stderr: &str) {
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();

    if !stdout.is_empty() {
        println!("{stdout}");
    }
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }
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

fn compact_progress_label(label: &str) -> String {
    const MAX_CHARS: usize = 64;
    let mut compact = label.replace('\n', " ");
    compact = compact.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = compact.chars().count();
    if count <= MAX_CHARS {
        compact
    } else {
        let shortened = compact.chars().take(MAX_CHARS - 1).collect::<String>();
        format!("{shortened}…")
    }
}

fn invocation_progress_label(invocation: &Invocation) -> String {
    let backend = invocation_backend_name(invocation);
    let args = invocation.args.as_slice();
    let target = invocation_target_label(invocation);

    if matches!(args, [action] if action == "update") && backend == "scoop" {
        return "Updating scoop buckets".to_string();
    }
    if matches!(args, [action, target] if action == "update" && target == "*") {
        return format!("Upgrading {backend} packages");
    }
    if matches!(args, [action, target] if action == "update" && target == "--global") {
        return "Upgrading npm global packages".to_string();
    }
    if args.first().map(String::as_str) == Some("update") {
        return match target {
            Some(target) => format!("Upgrading {backend}: {target}"),
            None => format!("Updating {backend} package"),
        };
    }
    if args.len() >= 2 && args[0] == "source" && args[1] == "update" {
        return format!("Updating {backend} sources");
    }
    if args.len() >= 2 && args[0] == "upgrade" && args[1] == "--all" {
        return format!("Upgrading {backend} packages");
    }
    if args.first().map(String::as_str) == Some("upgrade") {
        return match target {
            Some(target) => format!("Upgrading {backend}: {target}"),
            None => format!("Upgrading {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("install") {
        return match target {
            Some(target) => format!("Installing {backend}: {target}"),
            None => format!("Installing {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("uninstall") {
        return match target {
            Some(target) => format!("Removing {backend}: {target}"),
            None => format!("Removing {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("search") && args.len() >= 2 {
        return format!("Searching {backend} for {}", args[1]);
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "list" {
        if args.iter().any(|arg| arg == "--outdated") {
            return "Checking pip updates".to_string();
        }
        return "Listing pip packages".to_string();
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "install" {
        return match target {
            Some(target) if args.iter().any(|arg| arg == "--upgrade") => {
                format!("Upgrading pip: {target}")
            }
            Some(target) => format!("Installing pip: {target}"),
            None if args.iter().any(|arg| arg == "--upgrade") => {
                "Upgrading pip packages".to_string()
            }
            None => "Installing pip packages".to_string(),
        };
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "show" {
        return "Showing pip package details".to_string();
    }
    if args.len() >= 3
        && args[0] == "-c"
        && let Some(query) = args.last()
    {
        return format!("Searching pip for {query}");
    }
    if args.first().map(String::as_str) == Some("status") {
        return format!("Checking {backend} updates");
    }
    if args.first().map(String::as_str) == Some("list") {
        return format!("Listing {backend} packages");
    }
    if args.first().map(String::as_str) == Some("show")
        || args.first().map(String::as_str) == Some("info")
        || args.first().map(String::as_str) == Some("view")
    {
        return format!("Showing {backend} package details");
    }

    format!("Running {backend}")
}

fn invocation_target_label(invocation: &Invocation) -> Option<String> {
    let backend = invocation_backend_name(invocation);
    let args = invocation.args.as_slice();

    match backend {
        "winget" => argument_value_after(args, "--id")
            .or_else(|| argument_value_after(args, "--name"))
            .or_else(|| positional_argument(args, 1)),
        "scoop" | "choco" | "npm" => positional_argument(args, 1),
        "pip" => pip_invocation_target(args),
        _ => positional_argument(args, 1),
    }
}

fn argument_value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn positional_argument(args: &[String], start: usize) -> Option<String> {
    args.iter()
        .skip(start)
        .find(|arg| !arg.starts_with('-'))
        .cloned()
}

fn pip_invocation_target(args: &[String]) -> Option<String> {
    if args.len() < 4 || args[0] != "-m" || args[1] != "pip" {
        return None;
    }

    positional_argument(args, 3)
}

fn invocation_backend_name(invocation: &Invocation) -> &'static str {
    let program = invocation.program.replace('\\', "/").to_ascii_lowercase();
    if invocation.args.len() >= 2 && invocation.args[0] == "-m" && invocation.args[1] == "pip" {
        return "pip";
    }
    if program.contains("winget") {
        "winget"
    } else if program.contains("scoop") {
        "scoop"
    } else if program.contains("choco") {
        "choco"
    } else if program.contains("npm") {
        "npm"
    } else if program.contains("python") || program.ends_with("/py.exe") || program.ends_with("/py")
    {
        "pip"
    } else {
        "command"
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

fn print_package_details_sections(details: &[PackageDetails]) {
    let rendered = render_package_details_sections(details);
    if !rendered.is_empty() {
        println!();
        println!("{rendered}");
    }
}

fn render_package_details_sections(details: &[PackageDetails]) -> String {
    match details {
        [] => String::new(),
        [detail] => render_single_package_details(detail),
        _ => render_multi_package_details(details),
    }
}

fn render_single_package_details(detail: &PackageDetails) -> String {
    let mut lines = vec![format!("== {} ==", detail.backend)];
    append_package_detail_lines(&mut lines, detail);
    lines.join("\n")
}

fn render_multi_package_details(details: &[PackageDetails]) -> String {
    let field_labels = [
        "Name",
        "Version",
        "Summary",
        "Homepage",
        "License",
        "Author",
        "Repository",
        "Keywords",
        "Depends On",
    ];
    let label_width = field_labels
        .iter()
        .map(|label| label.len())
        .max()
        .unwrap_or(10)
        .max(10);
    let backend_width = details
        .iter()
        .map(|detail| detail.backend.to_string().len())
        .max()
        .unwrap_or(7)
        .max(7);

    let mut lines = vec!["== comparison ==".to_string()];
    for field in field_labels {
        let values: Vec<Option<String>> = details
            .iter()
            .map(|detail| package_detail_field_value(detail, field))
            .collect();
        let present_values: Vec<&String> = values.iter().filter_map(Option::as_ref).collect();
        if present_values.is_empty() {
            continue;
        }

        let first = present_values[0];
        let all_same = present_values.len() == details.len()
            && present_values.iter().all(|value| *value == first);
        if all_same {
            lines.push(format!("{field:<label_width$}: {first}"));
            continue;
        }

        lines.push(format!("{field:<label_width$}:"));
        for (detail, value) in details.iter().zip(values.iter()) {
            if let Some(value) = value {
                let backend = detail.backend.to_string();
                lines.push(format!(
                    "  {backend:<width$}  {value}",
                    width = backend_width
                ));
            }
        }
    }

    for detail in details {
        if detail.extra_fields.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(format!("== {} extras ==", detail.backend));
        for (label, value) in &detail.extra_fields {
            lines.push(format!("{label:<label_width$}: {value}"));
        }
    }

    lines.join("\n")
}

fn append_package_detail_lines(lines: &mut Vec<String>, detail: &PackageDetails) {
    lines.push(format!("{:<12}: {}", "Name", detail.name));
    lines.push(format!("{:<12}: {}", "Version", detail.version));
    if let Some(summary) = detail.summary.as_deref() {
        lines.push(format!("{:<12}: {}", "Summary", summary));
    }
    if let Some(homepage) = detail.homepage.as_deref() {
        lines.push(format!("{:<12}: {}", "Homepage", homepage));
    }
    if let Some(license) = detail.license.as_deref() {
        lines.push(format!("{:<12}: {}", "License", license));
    }
    if let Some(author) = detail.author.as_deref() {
        lines.push(format!("{:<12}: {}", "Author", author));
    }
    if let Some(repository) = detail.repository.as_deref() {
        lines.push(format!("{:<12}: {}", "Repository", repository));
    }
    if !detail.keywords.is_empty() {
        lines.push(format!(
            "{:<12}: {}",
            "Keywords",
            detail.keywords.join(", ")
        ));
    }
    if !detail.dependencies.is_empty() {
        lines.push(format!(
            "{:<12}: {}",
            "Depends On",
            detail.dependencies.join(", ")
        ));
    }
    for (label, value) in &detail.extra_fields {
        lines.push(format!("{label:<12}: {value}"));
    }
}

fn package_detail_field_value(detail: &PackageDetails, field: &str) -> Option<String> {
    match field {
        "Name" => Some(detail.name.clone()),
        "Version" => Some(detail.version.clone()),
        "Summary" => detail.summary.clone(),
        "Homepage" => detail.homepage.clone(),
        "License" => detail.license.clone(),
        "Author" => detail.author.clone(),
        "Repository" => detail.repository.clone(),
        "Keywords" if !detail.keywords.is_empty() => Some(detail.keywords.join(", ")),
        "Depends On" if !detail.dependencies.is_empty() => Some(detail.dependencies.join(", ")),
        _ => None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    backend: Option<Backend>,
    config_path: Option<PathBuf>,
    dry_run: bool,
    json: bool,
    assume_yes: Option<bool>,
    auto_elevate: Option<bool>,
    show_help: bool,
    show_version: bool,
    command: Subcommand,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut parser = ArgCursor::new(args.into_iter().collect());
        let mut backend = None;
        let mut config_path = None;
        let mut dry_run = false;
        let mut json = false;
        let mut assume_yes = None;
        let mut auto_elevate = None;
        let mut show_help = false;
        let mut show_version = false;

        while let Some(arg) = parser.peek() {
            match arg {
                "--backend" | "-b" => {
                    parser.next();
                    let value = parser
                        .next()
                        .ok_or_else(|| "missing value for --backend".to_string())?;
                    backend = Some(Backend::parse(&value)?);
                }
                "--config" => {
                    parser.next();
                    let value = parser
                        .next()
                        .ok_or_else(|| "missing value for --config".to_string())?;
                    config_path = Some(PathBuf::from(value));
                }
                "--dry-run" => {
                    parser.next();
                    dry_run = true;
                }
                "--json" => {
                    parser.next();
                    json = true;
                }
                "-y" | "--yes" => {
                    parser.next();
                    assume_yes = Some(true);
                }
                "--interactive" => {
                    parser.next();
                    assume_yes = Some(false);
                }
                "--elevate" => {
                    parser.next();
                    auto_elevate = Some(true);
                }
                "--no-elevate" => {
                    parser.next();
                    auto_elevate = Some(false);
                }
                "-h" | "--help" => {
                    parser.next();
                    show_help = true;
                }
                "-V" | "--version" => {
                    parser.next();
                    show_version = true;
                }
                "help" => {
                    parser.next();
                    show_help = true;
                }
                _ if arg.starts_with('-') => {
                    return Err(format!("unknown global option: {arg}"));
                }
                _ => break,
            }
        }

        let command = if show_help || show_version {
            Subcommand::Help
        } else {
            let name = parser
                .next()
                .ok_or_else(|| "missing command. Try --help".to_string())?;

            // Global flags are also accepted after the subcommand name, so
            // `waw list --json` behaves like `waw --json list`. They are pulled
            // out before the subcommand parser sees them.
            let (sub_args, trailing_globals) = split_global_flags(parser.remaining());
            for flag in trailing_globals {
                match flag.as_str() {
                    "--json" => json = true,
                    "--dry-run" => dry_run = true,
                    "-y" | "--yes" => assume_yes = Some(true),
                    "--interactive" => assume_yes = Some(false),
                    "--elevate" => auto_elevate = Some(true),
                    "--no-elevate" => auto_elevate = Some(false),
                    "-h" | "--help" => show_help = true,
                    "-V" => show_version = true,
                    _ => {}
                }
            }
            let mut sub_parser = ArgCursor::new(sub_args);
            let parsed = Subcommand::parse(name, &mut sub_parser)?;
            if sub_parser.has_remaining() {
                return Err(format!(
                    "unexpected trailing arguments: {}",
                    sub_parser.remaining().join(" ")
                ));
            }
            parsed
        };

        Ok(Self {
            backend,
            config_path,
            dry_run,
            json,
            assume_yes,
            auto_elevate,
            show_help,
            show_version,
            command,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Subcommand {
    Update,
    Upgrade { packages: Vec<String> },
    Install { mode: InstallMode },
    Remove { mode: RemoveMode },
    Hold { packages: Vec<String>, enable: bool },
    Search { query: String },
    List { upgradable: bool },
    Show { package: String },
    Backends,
    Backend { action: BackendAction },
    Export { path: Option<PathBuf> },
    Import { path: PathBuf },
    Help,
}

/// A package request that may or may not pin a version.
///
/// `raw` keeps exactly what the user typed so diagnostics echo it back, while
/// `name`/`version` hold the split form the backends need.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageSpec {
    name: String,
    version: Option<String>,
    raw: String,
}

impl PackageSpec {
    /// Splits `pkg@1.2.3` and `pkg==1.2.3` (pip style) into name and version.
    ///
    /// A leading `@` belongs to the scope of an npm package (`@scope/name`), so
    /// the separator search starts after it. The `==` form is checked first
    /// because pip uses it and a bare `@` inside a pip spec means something
    /// else entirely.
    fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();

        if let Some(index) = trimmed.find("==") {
            let (name, version) = trimmed.split_at(index);
            let version = &version[2..];
            if !name.is_empty() && !version.is_empty() {
                return Self {
                    name: name.to_string(),
                    version: Some(version.to_string()),
                    raw: trimmed.to_string(),
                };
            }
        }

        let search_from = usize::from(trimmed.starts_with('@'));
        if let Some(offset) = trimmed[search_from..].rfind('@') {
            let index = search_from + offset;
            let name = &trimmed[..index];
            let version = &trimmed[index + 1..];
            if !name.is_empty() && !version.is_empty() {
                return Self {
                    name: name.to_string(),
                    version: Some(version.to_string()),
                    raw: trimmed.to_string(),
                };
            }
        }

        Self {
            name: trimmed.to_string(),
            version: None,
            raw: trimmed.to_string(),
        }
    }

    fn without_version(raw: &str) -> Self {
        let parsed = Self::parse(raw);
        Self {
            name: parsed.name,
            version: None,
            raw: parsed.raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallMode {
    Search(String),
    Exact(Vec<PackageSpec>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoveMode {
    Search(String),
    Exact(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BackendAction {
    List,
    Enable { backend: Backend },
    Disable { backend: Backend },
    Install { backend: Backend, enable: bool },
    Default { backend: Option<Backend> },
}

impl Subcommand {
    fn parse(name: String, parser: &mut ArgCursor) -> Result<Self, String> {
        match name.as_str() {
            "update" => Ok(Self::Update),
            "upgrade" => {
                let mut packages = Vec::new();
                let mut upgrade_all = false;

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--all" => {
                            parser.next();
                            upgrade_all = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for upgrade: {value}"));
                        }
                        _ => packages.push(parser.next().expect("peeked value must exist")),
                    }
                }

                if upgrade_all && !packages.is_empty() {
                    return Err("--all cannot be combined with explicit package names".to_string());
                }

                Ok(Self::Upgrade {
                    packages: if upgrade_all { Vec::new() } else { packages },
                })
            }
            "install" => {
                enum ParsedInstallMode {
                    Search,
                    Exact,
                }

                let mut mode = None;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--pick" | "--select" | "--interactive" => {
                            parser.next();
                            if matches!(mode, Some(ParsedInstallMode::Exact)) {
                                return Err(
                                    "install cannot combine search-selection flags with --exact"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedInstallMode::Search);
                        }
                        "--exact" | "--direct" => {
                            parser.next();
                            if matches!(mode, Some(ParsedInstallMode::Search)) {
                                return Err(
                                    "install cannot combine --exact with search-selection flags"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedInstallMode::Exact);
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for install: {value}"));
                        }
                        _ => break,
                    }
                }

                let mode = match mode.unwrap_or(ParsedInstallMode::Search) {
                    ParsedInstallMode::Search => {
                        InstallMode::Search(parser.take_required_text("install")?)
                    }
                    ParsedInstallMode::Exact => InstallMode::Exact(
                        parser
                            .take_required_packages("install --exact")?
                            .into_iter()
                            .map(|raw| PackageSpec::parse(&raw))
                            .collect(),
                    ),
                };

                Ok(Self::Install { mode })
            }
            "remove" => {
                enum ParsedRemoveMode {
                    Search,
                    Exact,
                }

                let mut mode = None;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--pick" | "--select" | "--interactive" => {
                            parser.next();
                            if matches!(mode, Some(ParsedRemoveMode::Exact)) {
                                return Err(
                                    "remove cannot combine search-selection flags with --exact"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedRemoveMode::Search);
                        }
                        "--exact" | "--direct" => {
                            parser.next();
                            if matches!(mode, Some(ParsedRemoveMode::Search)) {
                                return Err(
                                    "remove cannot combine --exact with search-selection flags"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedRemoveMode::Exact);
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for remove: {value}"));
                        }
                        _ => break,
                    }
                }

                let mode = match mode.unwrap_or(ParsedRemoveMode::Search) {
                    ParsedRemoveMode::Search => {
                        RemoveMode::Search(parser.take_required_text("remove")?)
                    }
                    ParsedRemoveMode::Exact => {
                        RemoveMode::Exact(parser.take_required_packages("remove --exact")?)
                    }
                };

                Ok(Self::Remove { mode })
            }
            "hold" => {
                let mut enable = true;
                let mut packages = Vec::new();

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--off" | "--unhold" => {
                            parser.next();
                            enable = false;
                        }
                        "--on" => {
                            parser.next();
                            enable = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for hold: {value}"));
                        }
                        _ => packages.push(parser.next().expect("peeked value must exist")),
                    }
                }

                if packages.is_empty() {
                    return Err("hold requires at least one package".to_string());
                }

                Ok(Self::Hold { packages, enable })
            }
            "search" => Ok(Self::Search {
                query: parser.take_required_text("search")?,
            }),
            "list" => {
                let mut upgradable = false;

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--upgradable" | "--upgradeable" | "--updates" => {
                            parser.next();
                            upgradable = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for list: {value}"));
                        }
                        _ => {
                            return Err(format!(
                                "list does not accept positional arguments: {}",
                                parser.remaining().join(" ")
                            ));
                        }
                    }
                }

                Ok(Self::List { upgradable })
            }
            "show" => Ok(Self::Show {
                package: parser.take_required_single_value("show")?,
            }),
            "export" => {
                let mut path = None;
                while let Some(arg) = parser.peek() {
                    let arg = arg.to_string();
                    match arg.as_str() {
                        "--file" | "-o" => {
                            parser.next();
                            let value = parser
                                .next()
                                .ok_or_else(|| "export --file requires a path".to_string())?;
                            path = Some(PathBuf::from(value));
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for export: {value}"));
                        }
                        _ => {
                            let value = parser.next().expect("peeked value must exist");
                            path = Some(PathBuf::from(value));
                        }
                    }
                }
                Ok(Self::Export { path })
            }
            "import" => {
                let mut path = None;
                while let Some(arg) = parser.peek() {
                    let arg = arg.to_string();
                    match arg.as_str() {
                        "--file" | "-f" => {
                            parser.next();
                            let value = parser
                                .next()
                                .ok_or_else(|| "import --file requires a path".to_string())?;
                            path = Some(PathBuf::from(value));
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for import: {value}"));
                        }
                        _ => {
                            if path.is_some() {
                                return Err(
                                    "import accepts only one manifest path".to_string()
                                );
                            }
                            path = Some(PathBuf::from(
                                parser.next().expect("peeked value must exist"),
                            ));
                        }
                    }
                }
                let path = path.ok_or_else(|| {
                    "import requires a manifest path (or --file <path>)".to_string()
                })?;
                Ok(Self::Import { path })
            }
            "backends" => Ok(Self::Backends),
            "backend" => Ok(Self::Backend {
                action: BackendAction::parse(parser)?,
            }),
            other => Err(format!("unsupported command: {other}")),
        }
    }
}

impl BackendAction {
    fn parse(parser: &mut ArgCursor) -> Result<Self, String> {
        let action = parser.next().ok_or_else(|| {
            "backend requires a subcommand: list, enable, disable, install, default".to_string()
        })?;

        match action.as_str() {
            "list" => {
                if parser.has_remaining() {
                    return Err(format!(
                        "backend list does not accept extra arguments: {}",
                        parser.remaining().join(" ")
                    ));
                }
                Ok(Self::List)
            }
            "enable" => Ok(Self::Enable {
                backend: Backend::parse(&parser.take_required_single_value("backend enable")?)?,
            }),
            "disable" => Ok(Self::Disable {
                backend: Backend::parse(&parser.take_required_single_value("backend disable")?)?,
            }),
            "install" => {
                let mut enable = false;
                let backend_name = parser
                    .next()
                    .ok_or_else(|| "backend install requires exactly one backend".to_string())?;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--enable" => {
                            parser.next();
                            enable = true;
                        }
                        value => {
                            return Err(format!("unknown option for backend install: {value}"));
                        }
                    }
                }
                Ok(Self::Install {
                    backend: Backend::parse(&backend_name)?,
                    enable,
                })
            }
            "default" => {
                let value = parser.next().ok_or_else(|| {
                    "backend default requires a backend name or `auto`".to_string()
                })?;
                if parser.has_remaining() {
                    return Err(format!(
                        "backend default does not accept extra arguments: {}",
                        parser.remaining().join(" ")
                    ));
                }
                Ok(Self::Default {
                    backend: if value.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(Backend::parse(&value)?)
                    },
                })
            }
            other => Err(format!("unsupported backend subcommand: {other}")),
        }
    }
}

/// Options that are global but commonly typed after the subcommand, e.g.
/// `waw list --json`. This collects them out of the subcommand argument list.
///
/// The `--file`/`-f`/`-o` options take a value, so the token after them is left
/// alone; otherwise `waw export --file --json` would swallow a real path.
fn split_global_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    const VALUE_TAKING: [&str; 5] = ["--file", "-o", "-f", "--config", "--backend"];
    const GLOBAL: [&str; 10] = [
        "--json",
        "--dry-run",
        "-y",
        "--yes",
        "--interactive",
        "--elevate",
        "--no-elevate",
        "-h",
        "--help",
        "-V",
    ];

    let mut remaining = Vec::with_capacity(args.len());
    let mut globals = Vec::new();
    let mut skip_next_value = false;

    for arg in args {
        if skip_next_value {
            skip_next_value = false;
            remaining.push(arg.clone());
            continue;
        }
        if VALUE_TAKING.contains(&arg.as_str()) {
            skip_next_value = true;
            remaining.push(arg.clone());
            continue;
        }
        if GLOBAL.contains(&arg.as_str()) {
            globals.push(arg.clone());
            continue;
        }
        remaining.push(arg.clone());
    }

    (remaining, globals)
}

#[derive(Debug, Clone)]
struct ArgCursor {
    args: Vec<String>,
    index: usize,
}

impl ArgCursor {
    fn new(args: Vec<String>) -> Self {
        Self { args, index: 0 }
    }

    fn peek(&self) -> Option<&str> {
        self.args.get(self.index).map(String::as_str)
    }

    fn next(&mut self) -> Option<String> {
        let item = self.args.get(self.index).cloned();
        if item.is_some() {
            self.index += 1;
        }
        item
    }

    fn has_remaining(&self) -> bool {
        self.index < self.args.len()
    }

    fn remaining(&self) -> &[String] {
        &self.args[self.index..]
    }

    fn take_required_packages(&mut self, command: &str) -> Result<Vec<String>, String> {
        let mut packages = Vec::new();

        while let Some(arg) = self.peek() {
            if arg.starts_with('-') {
                return Err(format!("unknown option for {command}: {arg}"));
            }
            packages.push(self.next().expect("peeked value must exist"));
        }

        if packages.is_empty() {
            Err(format!("{command} requires at least one package"))
        } else {
            Ok(packages)
        }
    }

    fn take_required_text(&mut self, command: &str) -> Result<String, String> {
        let mut values = Vec::new();

        while let Some(arg) = self.peek() {
            if arg.starts_with('-') {
                return Err(format!("unknown option for {command}: {arg}"));
            }
            values.push(self.next().expect("peeked value must exist"));
        }

        if values.is_empty() {
            Err(format!("{command} requires a query"))
        } else {
            Ok(values.join(" "))
        }
    }

    fn take_required_single_value(&mut self, command: &str) -> Result<String, String> {
        let values = self.take_required_packages(command)?;
        if values.len() != 1 {
            Err(format!("{command} requires exactly one package"))
        } else {
            Ok(values.into_iter().next().expect("length already checked"))
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    backend: Option<Backend>,
    assume_yes: bool,
    auto_elevate: bool,
    winget_source: Option<String>,
    choco_source: Option<String>,
    scoop_bucket: Option<String>,
    pip_user: bool,
    command_timeout_seconds: u64,
    enable_winget: bool,
    enable_scoop: bool,
    enable_choco: bool,
    enable_npm: bool,
    enable_pip: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: None,
            assume_yes: true,
            auto_elevate: true,
            winget_source: None,
            choco_source: None,
            scoop_bucket: None,
            pip_user: true,
            command_timeout_seconds: DEFAULT_COMMAND_TIMEOUT_SECONDS,
            enable_winget: true,
            enable_scoop: true,
            enable_choco: true,
            enable_npm: true,
            enable_pip: true,
        }
    }
}

impl Config {
    fn load_from(path: &Path, explicit: bool) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == ErrorKind::NotFound && !explicit => Ok(Self::default()),
            Err(error) => Err(format!("failed to read config {}: {error}", path.display())),
        }
    }

    fn parse(contents: &str) -> Result<Self, String> {
        let mut config = Self::default();

        for (line_number, raw_line) in contents.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!(
                    "invalid config line {}: expected key = value",
                    line_number + 1
                )
            })?;
            let key = key.trim();
            let value = value.trim();

            match key {
                "backend" => config.backend = Some(Backend::parse(&parse_string(value)?)?),
                "assume_yes" => config.assume_yes = parse_bool(value)?,
                "auto_elevate" => config.auto_elevate = parse_bool(value)?,
                "winget_source" => config.winget_source = Some(parse_string(value)?),
                "choco_source" => config.choco_source = Some(parse_string(value)?),
                "scoop_bucket" => config.scoop_bucket = Some(parse_string(value)?),
                "pip_user" => config.pip_user = parse_bool(value)?,
            "command_timeout_seconds" => {
                config.command_timeout_seconds = value.parse::<u64>().map_err(|_| {
                    format!("invalid value for command_timeout_seconds: {value}")
                })?;
            }
                "enable_winget" => config.enable_winget = parse_bool(value)?,
                "enable_scoop" => config.enable_scoop = parse_bool(value)?,
                "enable_choco" => config.enable_choco = parse_bool(value)?,
                "enable_npm" => config.enable_npm = parse_bool(value)?,
                "enable_pip" => config.enable_pip = parse_bool(value)?,
                _ => {
                    return Err(format!(
                        "unsupported config key on line {}: {key}",
                        line_number + 1
                    ));
                }
            }
        }

        Ok(config)
    }

    fn winget_source(&self) -> Option<&str> {
        self.winget_source
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }

    fn choco_source(&self) -> Option<&str> {
        self.choco_source
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }

    fn qualify_scoop_package(&self, package: &str) -> String {
        match self
            .scoop_bucket
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(bucket) if !package.contains('/') => format!("{bucket}/{package}"),
            _ => package.to_string(),
        }
    }

    fn is_backend_enabled(&self, backend: Backend) -> bool {
        match backend {
            Backend::Winget => self.enable_winget,
            Backend::Scoop => self.enable_scoop,
            Backend::Chocolatey => self.enable_choco,
            Backend::Npm => self.enable_npm,
            Backend::Pip => self.enable_pip,
        }
    }

    fn set_backend_enabled(&mut self, backend: Backend, enabled: bool) {
        match backend {
            Backend::Winget => self.enable_winget = enabled,
            Backend::Scoop => self.enable_scoop = enabled,
            Backend::Chocolatey => self.enable_choco = enabled,
            Backend::Npm => self.enable_npm = enabled,
            Backend::Pip => self.enable_pip = enabled,
        }

        if !enabled && self.backend == Some(backend) {
            self.backend = None;
        }
    }

    fn to_toml(&self) -> String {
        let mut lines = Vec::new();
        if let Some(backend) = self.backend {
            lines.push(format!("backend = \"{backend}\""));
        }
        lines.push(format!("assume_yes = {}", self.assume_yes));
        lines.push(format!("auto_elevate = {}", self.auto_elevate));
        if let Some(source) = &self.winget_source {
            lines.push(format!("winget_source = {}", render_toml_string(source)));
        }
        if let Some(source) = &self.choco_source {
            lines.push(format!("choco_source = {}", render_toml_string(source)));
        }
        if let Some(bucket) = &self.scoop_bucket {
            lines.push(format!("scoop_bucket = {}", render_toml_string(bucket)));
        }
        lines.push(format!("pip_user = {}", self.pip_user));
        lines.push(format!(
            "command_timeout_seconds = {}",
            self.command_timeout_seconds
        ));
        lines.push(format!("enable_winget = {}", self.enable_winget));
        lines.push(format!("enable_scoop = {}", self.enable_scoop));
        lines.push(format!("enable_choco = {}", self.enable_choco));
        lines.push(format!("enable_npm = {}", self.enable_npm));
        lines.push(format!("enable_pip = {}", self.enable_pip));
        lines.join("\n") + "\n"
    }
}

#[derive(Debug, Clone)]
struct LoadedConfig {
    path: Option<PathBuf>,
    config: Config,
}

impl LoadedConfig {
    fn load(explicit_path: Option<&Path>) -> Result<Self, String> {
        let path = explicit_path
            .map(Path::to_path_buf)
            .or_else(default_config_path);
        let config = match explicit_path {
            Some(path) => Config::load_from(path, true)?,
            None => match path.as_deref() {
                Some(path) => Config::load_from(path, false)?,
                None => Config::default(),
            },
        };
        Ok(Self { path, config })
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Err("could not resolve a config path for saving".to_string());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create config directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::write(path, self.config.to_toml())
            .map_err(|error| format!("failed to write config {}: {error}", path.display()))
    }
}

#[derive(Debug, Clone)]
struct BackendStatus {
    backend: Backend,
    supported: bool,
    enabled: bool,
    available: bool,
    default_selected: bool,
}

fn collect_backend_statuses(config: &Config) -> Vec<BackendStatus> {
    [
        Backend::Winget,
        Backend::Scoop,
        Backend::Chocolatey,
        Backend::Npm,
        Backend::Pip,
    ]
    .into_iter()
    .map(|backend| BackendStatus {
        backend,
        supported: backend.supported_on_host(),
        enabled: config.is_backend_enabled(backend),
        available: backend.is_available(),
        default_selected: config.backend == Some(backend),
    })
    .collect()
}

fn print_backend_statuses(config: &Config) {
    for status in collect_backend_statuses(config) {
        print_backend_status_line(&status);
    }
}

fn print_backend_status(backend: Backend, config: &Config) {
    if let Some(status) = collect_backend_statuses(config)
        .into_iter()
        .find(|status| status.backend == backend)
    {
        print_backend_status_line(&status);
    }
}

fn print_backend_status_line(status: &BackendStatus) {
    let support = if status.supported {
        "supported"
    } else {
        "unsupported"
    };
    let enabled = if status.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let availability = if status.available {
        "available"
    } else {
        "missing"
    };
    let default_mark = if status.default_selected {
        " default"
    } else {
        ""
    };

    println!(
        "{:>10}: {support}, {enabled}, {availability}{default_mark}",
        status.backend
    );

    if !status.available {
        println!(
            "            install hint: {}",
            status.backend.install_hint()
        );
    }
}

fn render_backend_statuses_json(config: &Config) -> String {
    let items = collect_backend_statuses(config)
        .into_iter()
        .map(|status| render_backend_status_object(&status))
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!("[\n  {items}\n]")
}

fn render_backend_status_json(backend: Backend, config: &Config) -> String {
    collect_backend_statuses(config)
        .into_iter()
        .find(|status| status.backend == backend)
        .map(|status| render_backend_status_object(&status))
        .unwrap_or_else(|| "{}".to_string())
}

fn render_backend_status_object(status: &BackendStatus) -> String {
    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"supported\":{},",
            "\"enabled\":{},",
            "\"available\":{},",
            "\"default_selected\":{},",
            "\"install_hint\":\"{}\"",
            "}}"
        ),
        status.backend,
        status.supported,
        status.enabled,
        status.available,
        status.default_selected,
        escape_json_string(status.backend.install_hint()),
    )
}

fn render_backend_install_json(
    backend: Backend,
    available_before: bool,
    enable_requested: bool,
    dry_run: bool,
    result: &str,
    invocation: Option<&Invocation>,
) -> String {
    let command = invocation
        .map(Invocation::render_for_display)
        .unwrap_or_default();
    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"result\":\"{}\",",
            "\"available_before\":{},",
            "\"enable_requested\":{},",
            "\"dry_run\":{},",
            "\"bootstrap_command\":\"{}\",",
            "\"install_hint\":\"{}\"",
            "}}"
        ),
        backend,
        escape_json_string(result),
        available_before,
        enable_requested,
        dry_run,
        escape_json_string(&command),
        escape_json_string(backend.install_hint()),
    )
}

fn render_show_results_json(results: &[ShowBackendResult]) -> String {
    let items = results
        .iter()
        .map(render_show_result_json)
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!("[\n  {items}\n]")
}

fn render_list_results_json(rows: &[PackageListEntry], upgradable: bool) -> String {
    let items = rows
        .iter()
        .map(|row| {
            format!(
                "{{\"backend\":\"{}\",\"name\":\"{}\",\"package_id\":\"{}\",\"current_version\":\"{}\",\"available_version\":{}}}",
                escape_json_string(&row.backend.to_string()),
                escape_json_string(&row.name),
                escape_json_string(&row.package_id),
                escape_json_string(&row.current_version),
                render_json_optional_string(row.available_version.as_deref()),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n  ");
    let _ = upgradable;
    format!("[\n  {items}\n]")
}

fn render_search_results_json(candidates: &[SearchCandidate]) -> String {
    let items = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{{\"backend\":\"{}\",\"label\":\"{}\",\"install_id\":\"{}\",\"version\":{},\"source\":{}}}",
                escape_json_string(&candidate.backend.to_string()),
                escape_json_string(&candidate.label),
                escape_json_string(&candidate.install_id),
                render_json_optional_string(candidate.version.as_deref()),
                render_json_optional_string(candidate.source.as_deref()),
            )
        })
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!("[\n  {items}\n]")
}

fn render_show_result_json(result: &ShowBackendResult) -> String {
    let details = result
        .details
        .as_ref()
        .map(render_package_details_json)
        .unwrap_or_else(|| "null".to_string());
    let raw_output = result
        .raw_output
        .as_ref()
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .unwrap_or_else(|| "null".to_string());
    let error = result
        .error
        .as_ref()
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .unwrap_or_else(|| "null".to_string());

    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"command\":\"{}\",",
            "\"success\":{},",
            "\"dry_run\":{},",
            "\"details\":{},",
            "\"raw_output\":{},",
            "\"error\":{}",
            "}}"
        ),
        result.backend,
        escape_json_string(&result.command),
        result.success,
        result.dry_run,
        details,
        raw_output,
        error,
    )
}

fn render_package_details_json(details: &PackageDetails) -> String {
    let keywords = render_json_string_array(&details.keywords);
    let dependencies = render_json_string_array(&details.dependencies);
    let extra_fields = render_json_object(&details.extra_fields);
    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"name\":\"{}\",",
            "\"version\":\"{}\",",
            "\"summary\":{},",
            "\"homepage\":{},",
            "\"license\":{},",
            "\"author\":{},",
            "\"repository\":{},",
            "\"keywords\":{},",
            "\"dependencies\":{},",
            "\"extra_fields\":{}",
            "}}"
        ),
        details.backend,
        escape_json_string(&details.name),
        escape_json_string(&details.version),
        render_json_optional_string(details.summary.as_deref()),
        render_json_optional_string(details.homepage.as_deref()),
        render_json_optional_string(details.license.as_deref()),
        render_json_optional_string(details.author.as_deref()),
        render_json_optional_string(details.repository.as_deref()),
        keywords,
        dependencies,
        extra_fields,
    )
}

fn render_json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn render_json_string_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

fn render_json_object(entries: &[(String, String)]) -> String {
    let items = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "\"{}\":\"{}\"",
                escape_json_string(key),
                escape_json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{items}}}")
}

#[derive(Debug, Clone, Copy)]
struct RuntimeSettings<'a> {
    assume_yes: bool,
    auto_elevate: bool,
    command_timeout: Option<Duration>,
    config: &'a Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchCandidate {
    backend: Backend,
    label: String,
    install_id: String,
    version: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoveTarget {
    package: String,
    version: Option<String>,
}

impl RemoveTarget {
    fn unversioned(package: String) -> Self {
        Self {
            package,
            version: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageListEntry {
    backend: Backend,
    name: String,
    package_id: String,
    current_version: String,
    available_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageDetails {
    backend: Backend,
    name: String,
    version: String,
    summary: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    author: Option<String>,
    repository: Option<String>,
    keywords: Vec<String>,
    dependencies: Vec<String>,
    extra_fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShowBackendResult {
    backend: Backend,
    command: String,
    success: bool,
    dry_run: bool,
    details: Option<PackageDetails>,
    raw_output: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandCapture {
    stdout: String,
    stderr: String,
    success: bool,
    status_code: i32,
    timed_out: bool,
    /// The limit that applied to this run, for diagnostics. `None` means no
    /// limit was configured.
    timeout_seconds: Option<u64>,
}

struct CommandLogCapture {
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl CommandLogCapture {
    fn new() -> Result<Self, String> {
        let base = unique_capture_base("waw-command")?;
        Ok(Self {
            stdout_path: base.with_extension("stdout.log"),
            stderr_path: base.with_extension("stderr.log"),
        })
    }
}

impl Drop for CommandLogCapture {
    fn drop(&mut self) {
        // Removing here rather than at each success point keeps temporary files
        // from leaking when a command fails to spawn, a wait errors out, or any
        // other `?` returns early.
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

struct ProgressReporter {
    enabled: bool,
    frame_index: usize,
    last_width: usize,
    started_at: Instant,
}

impl ProgressReporter {
    fn new() -> Self {
        Self::new_with_enabled(true)
    }

    fn new_with_enabled(enabled: bool) -> Self {
        Self {
            enabled: enabled && io::stderr().is_terminal(),
            frame_index: 0,
            last_width: 0,
            started_at: Instant::now(),
        }
    }

    fn tick(&mut self, label: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
        let frame = FRAMES[self.frame_index % FRAMES.len()];
        self.frame_index += 1;
        self.render(&format!("{frame} {label}"))
    }

    fn finish(&mut self, success: bool, label: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let status = if success { "ok" } else { "failed" };
        self.render(&format!(
            "{status} {label} ({})",
            format_elapsed(self.started_at.elapsed())
        ))?;
        io::stderr()
            .write_all(b"\n")
            .and_then(|_| io::stderr().flush())
            .map_err(|error| format!("failed to update progress output: {error}"))?;
        self.last_width = 0;
        Ok(())
    }

    fn render(&mut self, message: &str) -> Result<(), String> {
        let padding = self.last_width.saturating_sub(message.chars().count());
        let padded = if padding == 0 {
            message.to_string()
        } else {
            format!("{message}{}", " ".repeat(padding))
        };
        self.last_width = message.chars().count();
        io::stderr()
            .write_all(format!("\r{padded}").as_bytes())
            .and_then(|_| io::stderr().flush())
            .map_err(|error| format!("failed to update progress output: {error}"))
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
const PIP_SEARCH_SCRIPT: &str = r#"import json, sys, urllib.error, urllib.request
query = sys.argv[1].strip()
normalized = query.lower().replace(" ", "-").replace("_", "-")

candidates = []
def add(name):
    lowered = name.lower()
    if lowered and lowered not in candidates:
        candidates.append(lowered)

add(normalized)
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
add(normalized + "2")
add(normalized + "3")
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
    return {
        "name": real_name,
        "version": info.get("version"),
        "summary": info.get("summary"),
    }

hits = []
for candidate in candidates:
    info = fetch(candidate)
    if info:
        hits.append(info)

hits.sort(
    key=lambda item: (
        item["name"].lower() != normalized,
        not item["name"].lower().startswith(normalized),
        len(item["name"]),
        item["name"].lower(),
    )
)
print(json.dumps(hits))
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: String,
    args: Vec<String>,
    message: Option<String>,
    requires_elevation: bool,
}

impl Invocation {
    fn owned(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_string(),
            args,
            message: None,
            requires_elevation: false,
        }
    }

    fn message(message: &str) -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            message: Some(message.to_string()),
            requires_elevation: false,
        }
    }

    fn with_elevation(mut self, requires_elevation: bool) -> Self {
        self.requires_elevation = requires_elevation;
        self
    }

    fn render_for_display(&self) -> String {
        if self.program.is_empty() {
            return String::new();
        }

        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(shell_escape(&self.program));
        parts.extend(self.args.iter().map(|arg| shell_escape(arg)));
        parts.join(" ")
    }
}

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
  search <query...>        Search the selected backend, or all enabled+available backends in auto mode
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
