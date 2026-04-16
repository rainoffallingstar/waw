use std::env;
use std::fmt::{self, Display};
use std::fs;
use std::io::ErrorKind;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

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

    if matches!(
        cli.command,
        Subcommand::Backends | Subcommand::Backend { action: _ }
    ) {
        return handle_backend_command(&cli.command, &mut loaded_config, cli.dry_run, cli.json);
    }

    let config = &loaded_config.config;
    let selected_backend = cli.backend.or(config.backend);

    if let Subcommand::Search { query } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_search(backends, query, &runtime_from(config, &cli));
    }

    if let Subcommand::Install {
        mode: InstallMode::Pick(query),
    } = &cli.command
    {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_interactive_install(backends, query, &runtime_from(config, &cli), cli.dry_run);
    }

    if let Subcommand::List { upgradable } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_list(
            backends,
            *upgradable,
            &runtime_from(config, &cli),
            cli.dry_run,
        );
    }

    if let Subcommand::Show { package } = &cli.command {
        let backends = resolve_auto_backends(selected_backend, config)?;
        return run_show(
            backends,
            package,
            &runtime_from(config, &cli),
            cli.dry_run,
            cli.json,
        );
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

    let runtime = runtime_from(config, &cli);

    if !cfg!(windows) && !cli.dry_run {
        eprintln!(
            "warning: {APP_NAME} is intended to run on Windows; execution may fail on this host"
        );
    }

    if let Subcommand::Upgrade { packages } = &cli.command {
        if backend == Backend::Pip && packages.is_empty() {
            return run_pip_upgrade_all(&runtime, cli.dry_run);
        }
    }

    let invocations = backend.plan(&cli.command, &runtime)?;
    execute_invocations(invocations, cli.dry_run)
}

fn handle_backend_command(
    command: &Subcommand,
    loaded_config: &mut LoadedConfig,
    dry_run: bool,
    json: bool,
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
            } else if let Some(invocation) = backend.install_invocation() {
                if !json {
                    println!("Bootstrap command for {backend}:");
                }
                execute_invocations(vec![invocation], dry_run)?;
                if json {
                    println!(
                        "{}",
                        render_backend_install_json(
                            *backend,
                            false,
                            *enable,
                            dry_run,
                            "bootstrap_requested",
                            backend.install_invocation().as_ref()
                        )
                    );
                }
            } else {
                if json {
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
        assume_yes: cli.assume_yes || config.assume_yes,
        config,
    }
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

fn execute_invocations(invocations: Vec<Invocation>, dry_run: bool) -> Result<ExitCode, String> {
    for invocation in invocations {
        if let Some(message) = invocation.message.as_deref() {
            println!("{message}");
        }

        if invocation.program.is_empty() {
            continue;
        }

        println!("> {}", invocation.render_for_display());

        if dry_run {
            continue;
        }

        let display_command = invocation.render_for_display();
        let status = Command::new(&invocation.program)
            .args(&invocation.args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|error| format!("failed to launch {}: {error}", invocation.program))?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(format!(
                "backend command failed with exit code {code}: {display_command}"
            ));
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn run_interactive_install(
    backends: Vec<Backend>,
    query: &str,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let candidates = collect_search_candidates(&backends, query, runtime)?;
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

    let selected_indices = prompt_for_selection(candidates.len())?;
    if selected_indices.is_empty() {
        println!("No selection made. Nothing to install.");
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
            invocations.extend(backend.plan_install(&packages, runtime));
        }
    }

    execute_invocations(invocations, dry_run)
}

fn run_pip_upgrade_all(runtime: &RuntimeSettings<'_>, dry_run: bool) -> Result<ExitCode, String> {
    let capture_invocation = Backend::Pip.pip_outdated_invocation();
    println!("Resolving outdated pip packages...");
    println!("> {}", capture_invocation.render_for_display());

    if dry_run {
        println!(
            "dry-run: would query outdated pip packages first, then print install --upgrade commands."
        );
    }

    let output = run_capture(&capture_invocation)?;
    let packages = parse_pip_outdated_json_names(&output);
    if packages.is_empty() {
        println!("No outdated pip packages found.");
        return Ok(ExitCode::SUCCESS);
    }

    let invocations = Backend::Pip.plan_install_or_upgrade(&packages, runtime, true);
    execute_invocations(invocations, dry_run)
}

fn run_search(
    backends: Vec<Backend>,
    query: &str,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let candidates = collect_search_candidates(&backends, query, runtime)?;
    if candidates.is_empty() {
        println!("No matching packages found.");
    } else {
        print_candidates(&candidates);
    }
    Ok(ExitCode::SUCCESS)
}

fn run_list(
    backends: Vec<Backend>,
    upgradable: bool,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
) -> Result<ExitCode, String> {
    let single_backend = backends.len() == 1;
    let noun = if upgradable {
        "upgradable packages"
    } else {
        "installed packages"
    };
    let mut parsed_rows = Vec::new();
    let mut raw_sections = Vec::new();

    for backend in backends {
        let invocation = backend
            .plan_list(upgradable, runtime)
            .into_iter()
            .next()
            .expect("list should always produce one invocation");
        println!("Listing {noun} from {backend}");
        println!("> {}", invocation.render_for_display());

        if dry_run {
            continue;
        }

        let capture = run_capture_detailed(&invocation)?;
        if !backend.accepts_list_capture(upgradable, &capture) {
            if single_backend {
                return Err(render_command_failure(&invocation, &capture));
            }
            eprintln!(
                "warning: failed to list {noun} from {backend}: {}",
                render_command_failure(&invocation, &capture)
            );
            continue;
        }

        if let Some(rows) = backend.parse_list_entries(upgradable, &capture.stdout) {
            parsed_rows.extend(rows);
        } else if let Some(section) = render_backend_output_section(backend, &capture.stdout) {
            raw_sections.push(section);
        }
    }

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    let parsed_rows = sort_list_rows(parsed_rows);
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
            println!("> {}", invocation.render_for_display());
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

        let capture = run_capture_detailed(&invocation)?;
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
            if single_backend {
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

fn collect_search_candidates(
    backends: &[Backend],
    query: &str,
    runtime: &RuntimeSettings<'_>,
) -> Result<Vec<SearchCandidate>, String> {
    let mut candidates = Vec::new();
    let aggregate = backends.len() > 1;

    for backend in backends {
        let search_invocation = backend.search_invocation(query, runtime);
        if aggregate {
            println!("Searching {backend} for: {query}");
        } else {
            println!("Searching candidates for: {query}");
        }
        println!("> {}", search_invocation.render_for_display());

        let search_output = run_capture(&search_invocation)?;
        candidates.extend(backend.parse_search_candidates(&search_output));
    }

    let candidates = dedupe_search_candidates(candidates);
    let candidates = sort_search_candidates(candidates, query, backends);

    Ok(candidates)
}

fn run_capture(invocation: &Invocation) -> Result<String, String> {
    let capture = run_capture_detailed(invocation)?;
    if !capture.success {
        return Err(render_command_failure(invocation, &capture));
    }

    Ok(capture.stdout)
}

fn run_capture_detailed(invocation: &Invocation) -> Result<CommandCapture, String> {
    let output = Command::new(&invocation.program)
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to launch {}: {error}", invocation.program))?;

    Ok(CommandCapture {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
        status_code: output.status.code().unwrap_or(1),
    })
}

fn render_command_failure(invocation: &Invocation, capture: &CommandCapture) -> String {
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
        let all_same = present_values.iter().all(|value| *value == first);
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

fn prompt_for_selection(max: usize) -> Result<Vec<usize>, String> {
    print!("Select packages to install (e.g. 1 2 5, 1,3-4; empty to cancel): ");
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
    assume_yes: bool,
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
        let mut assume_yes = false;
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
                    assume_yes = true;
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
            Subcommand::parse(name, &mut parser)?
        };

        if parser.has_remaining() {
            return Err(format!(
                "unexpected trailing arguments: {}",
                parser.remaining().join(" ")
            ));
        }

        Ok(Self {
            backend,
            config_path,
            dry_run,
            json,
            assume_yes,
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
    Remove { packages: Vec<String> },
    Hold { packages: Vec<String>, enable: bool },
    Search { query: String },
    List { upgradable: bool },
    Show { package: String },
    Backends,
    Backend { action: BackendAction },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InstallMode {
    Packages(Vec<String>),
    Pick(String),
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
                let mut pick = false;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--pick" | "--select" | "--interactive" => {
                            parser.next();
                            pick = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for install: {value}"));
                        }
                        _ => break,
                    }
                }

                let mode = if pick {
                    InstallMode::Pick(parser.take_required_text("install --pick")?)
                } else {
                    InstallMode::Packages(parser.take_required_packages("install")?)
                };

                Ok(Self::Install { mode })
            }
            "remove" => Ok(Self::Remove {
                packages: parser.take_required_packages("remove")?,
            }),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Winget,
    Scoop,
    Chocolatey,
    Npm,
    Pip,
}

impl Backend {
    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "winget" => Ok(Self::Winget),
            "scoop" => Ok(Self::Scoop),
            "choco" | "chocolatey" => Ok(Self::Chocolatey),
            "npm" => Ok(Self::Npm),
            "pip" => Ok(Self::Pip),
            _ => Err(format!(
                "unsupported backend: {value}. Expected one of: winget, scoop, choco, npm, pip"
            )),
        }
    }

    fn detect(config: &Config) -> Result<Self, String> {
        let candidates = [
            Self::Winget,
            Self::Scoop,
            Self::Chocolatey,
            Self::Npm,
            Self::Pip,
        ];
        candidates
            .into_iter()
            .find(|backend| config.is_backend_enabled(*backend) && backend.is_available())
            .ok_or_else(|| {
                "no enabled backend was found in PATH. Enable a backend or install winget, scoop, choco, npm, or pip.".to_string()
            })
    }

    fn is_available(self) -> bool {
        let candidates = self.command_candidates();
        candidates.into_iter().any(|(program, args)| {
            Command::new(&program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
    }

    fn command_candidates(self) -> Vec<(String, Vec<String>)> {
        match self {
            Self::Winget => vec![("winget".to_string(), vec!["--version".to_string()])],
            Self::Scoop => vec![("scoop".to_string(), vec!["--version".to_string()])],
            Self::Chocolatey => vec![("choco".to_string(), vec!["-v".to_string()])],
            Self::Npm => vec![(
                if cfg!(windows) { "npm.cmd" } else { "npm" }.to_string(),
                vec!["--version".to_string()],
            )],
            Self::Pip => vec![
                (
                    "python".to_string(),
                    vec!["-m".to_string(), "pip".to_string(), "--version".to_string()],
                ),
                (
                    "python3".to_string(),
                    vec!["-m".to_string(), "pip".to_string(), "--version".to_string()],
                ),
                ("pip".to_string(), vec!["--version".to_string()]),
            ],
        }
    }

    fn supported_on_host(self) -> bool {
        match self {
            Self::Winget | Self::Scoop | Self::Chocolatey => cfg!(windows),
            Self::Npm | Self::Pip => true,
        }
    }

    fn install_hint(self) -> &'static str {
        match self {
            Self::Winget => {
                "Install Microsoft App Installer / WinGet from the Microsoft Store or official package manager distribution."
            }
            Self::Scoop => {
                r#"powershell -NoProfile -ExecutionPolicy Bypass -Command "iwr -useb get.scoop.sh | iex""#
            }
            Self::Chocolatey => {
                r#"powershell -NoProfile -ExecutionPolicy Bypass -Command "Set-ExecutionPolicy Bypass -Scope Process -Force; [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))""#
            }
            Self::Npm => "Install Node.js, for example: winget install OpenJS.NodeJS.LTS",
            Self::Pip => "Install Python, for example: winget install Python.Python.3.12",
        }
    }

    fn install_invocation(self) -> Option<Invocation> {
        match self {
            Self::Winget => None,
            Self::Scoop => Some(Invocation::owned(
                "powershell",
                vec![
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-Command".to_string(),
                    "iwr -useb get.scoop.sh | iex".to_string(),
                ],
            )),
            Self::Chocolatey => Some(Invocation::owned(
                "powershell",
                vec![
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-Command".to_string(),
                    "Set-ExecutionPolicy Bypass -Scope Process -Force; [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))".to_string(),
                ],
            )),
            Self::Npm => Some(Invocation::owned(
                "winget",
                vec![
                    "install".to_string(),
                    "--id".to_string(),
                    "OpenJS.NodeJS.LTS".to_string(),
                    "--exact".to_string(),
                ],
            )),
            Self::Pip => Some(Invocation::owned(
                "winget",
                vec![
                    "install".to_string(),
                    "--id".to_string(),
                    "Python.Python.3.12".to_string(),
                    "--exact".to_string(),
                ],
            )),
        }
    }

    fn base_invocation(self, args: Vec<String>) -> Invocation {
        match self {
            Self::Winget => Invocation::owned("winget", args),
            Self::Scoop => Invocation::owned("scoop", args),
            Self::Chocolatey => Invocation::owned("choco", args),
            Self::Npm => Invocation::owned(if cfg!(windows) { "npm.cmd" } else { "npm" }, args),
            Self::Pip => {
                let program = if command_works(
                    "python",
                    &["-m".to_string(), "pip".to_string(), "--version".to_string()],
                ) {
                    "python"
                } else if command_works(
                    "python3",
                    &["-m".to_string(), "pip".to_string(), "--version".to_string()],
                ) {
                    "python3"
                } else {
                    "python"
                };
                let mut pip_args = vec!["-m".to_string(), "pip".to_string()];
                pip_args.extend(args);
                Invocation::owned(program, pip_args)
            }
        }
    }

    fn plan(
        self,
        command: &Subcommand,
        runtime: &RuntimeSettings<'_>,
    ) -> Result<Vec<Invocation>, String> {
        match command {
            Subcommand::Update => Ok(self.plan_update(runtime)),
            Subcommand::Upgrade { packages } => Ok(self.plan_upgrade(packages, runtime)),
            Subcommand::Install {
                mode: InstallMode::Packages(packages),
            } => Ok(self.plan_install(packages, runtime)),
            Subcommand::Install {
                mode: InstallMode::Pick(_),
            } => Ok(Vec::new()),
            Subcommand::Remove { packages } => Ok(self.plan_remove(packages, runtime)),
            Subcommand::Hold { packages, enable } => Ok(self.plan_hold(packages, *enable)),
            Subcommand::Search { query } => Ok(self.plan_search(query, runtime)),
            Subcommand::List { upgradable } => Ok(self.plan_list(*upgradable, runtime)),
            Subcommand::Show { package } => Ok(self.plan_show(package, runtime)),
            Subcommand::Backends | Subcommand::Backend { action: _ } => Ok(Vec::new()),
            Subcommand::Help => Ok(Vec::new()),
        }
    }

    fn plan_update(self, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                let mut args = vec!["source".to_string(), "update".to_string()];
                if let Some(source) = runtime.config.winget_source() {
                    args.push("--name".to_string());
                    args.push(source.to_string());
                }
                vec![self.base_invocation(args)]
            }
            Self::Scoop => vec![self.base_invocation(vec!["update".to_string()])],
            Self::Chocolatey => vec![Invocation::message(
                "Chocolatey does not expose a dedicated apt-get-style index refresh. Skipping update as a no-op.",
            )],
            Self::Npm => vec![Invocation::message(
                "npm does not expose a separate apt-get-style update step. Skipping update as a no-op.",
            )],
            Self::Pip => vec![Invocation::message(
                "pip does not expose a separate apt-get-style update step. Skipping update as a no-op.",
            )],
        }
    }

    fn plan_upgrade(self, packages: &[String], runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                if packages.is_empty() {
                    let mut args = vec![
                        "upgrade".to_string(),
                        "--all".to_string(),
                        "--include-unknown".to_string(),
                        "--accept-source-agreements".to_string(),
                        "--accept-package-agreements".to_string(),
                    ];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.base_invocation(args)]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            let mut args = vec![
                                "upgrade".to_string(),
                                "--id".to_string(),
                                package.clone(),
                                "--exact".to_string(),
                                "--accept-source-agreements".to_string(),
                                "--accept-package-agreements".to_string(),
                            ];
                            append_source_arg(&mut args, runtime.config.winget_source());
                            if runtime.assume_yes {
                                args.push("--silent".to_string());
                            }
                            self.base_invocation(args)
                        })
                        .collect()
                }
            }
            Self::Scoop => {
                if packages.is_empty() {
                    vec![self.base_invocation(vec!["update".to_string(), "*".to_string()])]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            self.base_invocation(vec![
                                "update".to_string(),
                                runtime.config.qualify_scoop_package(package),
                            ])
                        })
                        .collect()
                }
            }
            Self::Chocolatey => {
                if packages.is_empty() {
                    let mut args = vec!["upgrade".to_string(), "all".to_string()];
                    append_source_arg(&mut args, runtime.config.choco_source());
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    vec![self.base_invocation(args)]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            let mut args = vec!["upgrade".to_string(), package.clone()];
                            append_source_arg(&mut args, runtime.config.choco_source());
                            if runtime.assume_yes {
                                args.push("-y".to_string());
                            }
                            self.base_invocation(args)
                        })
                        .collect()
                }
            }
            Self::Npm => {
                if packages.is_empty() {
                    vec![self.base_invocation(vec!["update".to_string(), "--global".to_string()])]
                } else {
                    self.plan_install_or_upgrade(packages, runtime, true)
                }
            }
            Self::Pip => self.plan_install_or_upgrade(packages, runtime, true),
        }
    }

    fn plan_install(self, packages: &[String], runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        self.plan_install_or_upgrade(packages, runtime, false)
    }

    fn plan_install_or_upgrade(
        self,
        packages: &[String],
        runtime: &RuntimeSettings<'_>,
        upgrade: bool,
    ) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        "install".to_string(),
                        "--id".to_string(),
                        package.clone(),
                        "--exact".to_string(),
                        "--accept-source-agreements".to_string(),
                        "--accept-package-agreements".to_string(),
                    ];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    if runtime.assume_yes {
                        args.push("--silent".to_string());
                    }
                    self.base_invocation(args)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        if upgrade {
                            "update".to_string()
                        } else {
                            "install".to_string()
                        },
                        runtime.config.qualify_scoop_package(package),
                    ])
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        if upgrade {
                            "upgrade".to_string()
                        } else {
                            "install".to_string()
                        },
                        package.clone(),
                    ];
                    append_source_arg(&mut args, runtime.config.choco_source());
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    self.base_invocation(args)
                })
                .collect(),
            Self::Npm => packages
                .iter()
                .map(|package| {
                    let versioned = if upgrade {
                        format!("{package}@latest")
                    } else {
                        package.clone()
                    };
                    self.base_invocation(vec![
                        "install".to_string(),
                        versioned,
                        "--global".to_string(),
                    ])
                })
                .collect(),
            Self::Pip => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        "install".to_string(),
                        package.clone(),
                        "--no-input".to_string(),
                    ];
                    if upgrade {
                        args.push("--upgrade".to_string());
                    }
                    if runtime.config.pip_user {
                        args.push("--user".to_string());
                    }
                    self.base_invocation(args)
                })
                .collect(),
        }
    }

    fn plan_remove(self, packages: &[String], runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        "uninstall".to_string(),
                        "--id".to_string(),
                        package.clone(),
                        "--exact".to_string(),
                    ];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    self.base_invocation(args)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        "uninstall".to_string(),
                        runtime.config.qualify_scoop_package(package),
                    ])
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    let mut args = vec!["uninstall".to_string(), package.clone()];
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    self.base_invocation(args)
                })
                .collect(),
            Self::Npm => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        "uninstall".to_string(),
                        package.clone(),
                        "--global".to_string(),
                    ])
                })
                .collect(),
            Self::Pip => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        "uninstall".to_string(),
                        package.clone(),
                        "--no-input".to_string(),
                        "--yes".to_string(),
                    ])
                })
                .collect(),
        }
    }

    fn plan_hold(self, packages: &[String], enable: bool) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let action = if enable { "add" } else { "remove" };
                    let mut args = vec![
                        "pin".to_string(),
                        action.to_string(),
                        "--id".to_string(),
                        package.clone(),
                    ];
                    if enable {
                        args.push("--blocking".to_string());
                    }
                    args.push("--exact".to_string());
                    self.base_invocation(args)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        if enable { "hold" } else { "unhold" }.to_string(),
                        package.clone(),
                    ])
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    self.base_invocation(vec![
                        "pin".to_string(),
                        if enable { "add" } else { "remove" }.to_string(),
                        "--name".to_string(),
                        package.clone(),
                    ])
                })
                .collect(),
            Self::Npm | Self::Pip => vec![Invocation::message(
                "This backend does not support a native hold/pin operation.",
            )],
        }
    }

    fn plan_search(self, query: &str, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        vec![self.search_invocation(query, runtime)]
    }

    fn search_invocation(self, query: &str, runtime: &RuntimeSettings<'_>) -> Invocation {
        match self {
            Self::Winget => {
                let mut args = vec!["search".to_string(), query.to_string()];
                append_source_arg(&mut args, runtime.config.winget_source());
                self.base_invocation(args)
            }
            Self::Scoop => self.base_invocation(vec!["search".to_string(), query.to_string()]),
            Self::Chocolatey => {
                let mut args = vec![
                    "search".to_string(),
                    query.to_string(),
                    "--limit-output".to_string(),
                ];
                append_source_arg(&mut args, runtime.config.choco_source());
                self.base_invocation(args)
            }
            Self::Npm => self.base_invocation(vec![
                "search".to_string(),
                query.to_string(),
                "--json".to_string(),
            ]),
            Self::Pip => self.pip_search_invocation(query),
        }
    }

    fn parse_search_candidates(self, output: &str) -> Vec<SearchCandidate> {
        let mut candidates = match self {
            Self::Winget => parse_winget_search_candidates(output),
            Self::Scoop => parse_scoop_search_candidates(output),
            Self::Chocolatey => parse_choco_search_candidates(output),
            Self::Npm => parse_npm_search_candidates(output),
            Self::Pip => parse_pip_search_candidates(output),
        };
        for candidate in &mut candidates {
            candidate.backend = self;
        }
        candidates
    }

    fn plan_list(self, upgradable: bool, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                if upgradable {
                    let mut args = vec!["upgrade".to_string()];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.base_invocation(args)]
                } else {
                    let mut args = vec!["list".to_string()];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.base_invocation(args)]
                }
            }
            Self::Scoop => vec![self.base_invocation(vec![if upgradable {
                "status".to_string()
            } else {
                "list".to_string()
            }])],
            Self::Chocolatey => {
                let mut args = if upgradable {
                    vec!["outdated".to_string(), "--limit-output".to_string()]
                } else {
                    vec![
                        "list".to_string(),
                        "--local-only".to_string(),
                        "--limit-output".to_string(),
                    ]
                };
                append_source_arg(&mut args, runtime.config.choco_source());
                vec![self.base_invocation(args)]
            }
            Self::Npm => vec![self.base_invocation(if upgradable {
                vec![
                    "outdated".to_string(),
                    "--json".to_string(),
                    "--global".to_string(),
                ]
            } else {
                vec![
                    "list".to_string(),
                    "--json".to_string(),
                    "--depth=0".to_string(),
                    "--global".to_string(),
                ]
            })],
            Self::Pip => vec![self.base_invocation(if upgradable {
                vec![
                    "list".to_string(),
                    "--outdated".to_string(),
                    "--format=json".to_string(),
                ]
            } else {
                vec!["list".to_string(), "--format=json".to_string()]
            })],
        }
    }

    fn accepts_list_capture(self, upgradable: bool, capture: &CommandCapture) -> bool {
        capture.success || matches!(self, Self::Npm if upgradable && capture.status_code == 1)
    }

    fn parse_list_entries(self, upgradable: bool, output: &str) -> Option<Vec<PackageListEntry>> {
        match (self, upgradable) {
            (Self::Winget, false) => parse_winget_list_entries(output),
            (Self::Winget, true) => parse_winget_upgrade_entries(output),
            (Self::Scoop, false) => parse_scoop_list_entries(output),
            (Self::Scoop, true) => parse_scoop_upgrade_entries(output),
            (Self::Chocolatey, false) => parse_choco_list_entries(output),
            (Self::Chocolatey, true) => parse_choco_upgrade_entries(output),
            (Self::Npm, false) => parse_npm_list_entries(output),
            (Self::Npm, true) => parse_npm_upgrade_entries(output),
            (Self::Pip, false) => parse_pip_list_entries(output),
            (Self::Pip, true) => parse_pip_upgrade_entries(output),
        }
    }

    fn parse_show_details(self, output: &str) -> Option<PackageDetails> {
        match self {
            Self::Npm => parse_npm_show_details(output),
            Self::Pip => parse_pip_show_details(output),
            Self::Winget | Self::Scoop | Self::Chocolatey => parse_key_value_show_details(output)
                .map(|mut details| {
                    details.backend = self;
                    details
                }),
        }
    }

    fn plan_show(self, package: &str, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                let mut args = vec![
                    "show".to_string(),
                    "--id".to_string(),
                    package.to_string(),
                    "--exact".to_string(),
                ];
                append_source_arg(&mut args, runtime.config.winget_source());
                vec![self.base_invocation(args)]
            }
            Self::Scoop => vec![self.base_invocation(vec![
                "info".to_string(),
                runtime.config.qualify_scoop_package(package),
            ])],
            Self::Chocolatey => {
                let mut args = vec!["info".to_string(), package.to_string()];
                append_source_arg(&mut args, runtime.config.choco_source());
                vec![self.base_invocation(args)]
            }
            Self::Npm => vec![self.base_invocation(vec!["view".to_string(), package.to_string()])],
            Self::Pip => vec![self.base_invocation(vec!["show".to_string(), package.to_string()])],
        }
    }

    fn pip_search_invocation(self, query: &str) -> Invocation {
        let python = self.pip_python_program();
        Invocation::owned(
            &python,
            vec![
                "-c".to_string(),
                PIP_SEARCH_SCRIPT.to_string(),
                query.to_string(),
            ],
        )
    }

    fn pip_outdated_invocation(self) -> Invocation {
        self.base_invocation(vec![
            "list".to_string(),
            "--outdated".to_string(),
            "--format=json".to_string(),
        ])
    }

    fn pip_python_program(self) -> String {
        if command_works(
            "python",
            &["-m".to_string(), "pip".to_string(), "--version".to_string()],
        ) {
            "python".to_string()
        } else if command_works(
            "python3",
            &["-m".to_string(), "pip".to_string(), "--version".to_string()],
        ) {
            "python3".to_string()
        } else {
            "python".to_string()
        }
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
    winget_source: Option<String>,
    choco_source: Option<String>,
    scoop_bucket: Option<String>,
    pip_user: bool,
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
            assume_yes: false,
            winget_source: None,
            choco_source: None,
            scoop_bucket: None,
            pip_user: true,
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
                "winget_source" => config.winget_source = Some(parse_string(value)?),
                "choco_source" => config.choco_source = Some(parse_string(value)?),
                "scoop_bucket" => config.scoop_bucket = Some(parse_string(value)?),
                "pip_user" => config.pip_user = parse_bool(value)?,
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
struct PackageListEntry {
    backend: Backend,
    name: String,
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
}

const PIP_SEARCH_SCRIPT: &str = r#"import json, sys, urllib.request
query = sys.argv[1].strip().lower()
req = urllib.request.Request("https://pypi.org/simple/", headers={"Accept": "application/vnd.pypi.simple.v1+json"})
with urllib.request.urlopen(req) as resp:
    data = json.load(resp)
names = [p.get("name") for p in data.get("projects", []) if isinstance(p, dict) and p.get("name")]
matches = [n for n in names if query in n.lower()]
matches.sort(key=lambda n: (not n.lower().startswith(query), len(n), n.lower()))
for name in matches[:20]:
    print(name)
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Invocation {
    program: String,
    args: Vec<String>,
    message: Option<String>,
}

impl Invocation {
    fn owned(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_string(),
            args,
            message: None,
        }
    }

    fn message(message: &str) -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            message: Some(message.to_string()),
        }
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

fn parse_winget_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let mut candidates = Vec::new();
    let mut previous_line = "";
    let mut dashed_header_seen = false;
    let mut id_index = None;
    let mut version_index = None;
    let mut source_index = None;

    for line in output.lines() {
        if !dashed_header_seen && line.contains("---") {
            id_index = find_char_index(previous_line, "Id");
            version_index = find_char_index(previous_line, "Version");
            source_index = find_char_index(previous_line, "Source");
            dashed_header_seen = true;
            continue;
        }

        if dashed_header_seen {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let (Some(id_index), Some(version_index)) = (id_index, version_index) {
                let name = slice_chars(line, 0, id_index).trim().to_string();
                let id_end = version_index;
                let id = slice_chars(line, id_index, id_end).trim().to_string();
                let version_end = source_index.unwrap_or_else(|| line.chars().count());
                let version = slice_chars(line, version_index, version_end)
                    .trim()
                    .to_string();
                let source = source_index
                    .map(|start| {
                        slice_chars(line, start, line.chars().count())
                            .trim()
                            .to_string()
                    })
                    .filter(|value| !value.is_empty());

                if !id.is_empty() {
                    candidates.push(SearchCandidate {
                        backend: Backend::Winget,
                        label: if name.is_empty() { id.clone() } else { name },
                        install_id: id,
                        version: if version.is_empty() {
                            None
                        } else {
                            Some(version)
                        },
                        source,
                    });
                }
            }
        }

        previous_line = line;
    }

    candidates
}

fn parse_scoop_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let mut candidates = Vec::new();
    let mut bucket: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('\'') {
            bucket = trimmed
                .split(' ')
                .next()
                .map(|value| value.trim_matches('\'').to_string());
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let id = parts[0];
        let version = parts[1];
        if matches!(id, "WARN" | "No") || matches!(version, "ignored" | "Matches") {
            continue;
        }

        let source = bucket.clone();
        let install_id = match source.as_deref() {
            Some(source) => format!("{source}/{id}"),
            None => id.to_string(),
        };

        candidates.push(SearchCandidate {
            backend: Backend::Scoop,
            label: id.to_string(),
            install_id,
            version: Some(
                version
                    .trim_matches(|ch| ch == '(' || ch == ')')
                    .to_string(),
            ),
            source,
        });
    }

    candidates
}

fn parse_choco_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let mut candidates = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Chocolatey v") {
            continue;
        }

        if let Some((id, version)) = trimmed.split_once('|') {
            let id = id.trim();
            let version = version.trim();
            if !id.is_empty() {
                candidates.push(SearchCandidate {
                    backend: Backend::Chocolatey,
                    label: id.to_string(),
                    install_id: id.to_string(),
                    version: if version.is_empty() {
                        None
                    } else {
                        Some(version.to_string())
                    },
                    source: None,
                });
            }
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 || !starts_with_version_char(parts[1]) {
            continue;
        }

        candidates.push(SearchCandidate {
            backend: Backend::Chocolatey,
            label: parts[0].to_string(),
            install_id: parts[0].to_string(),
            version: Some(parts[1].to_string()),
            source: None,
        });
    }

    candidates
}

fn parse_npm_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    let array_start = trimmed.find('[');
    if let Some(array_start) = array_start {
        let json_segment = &trimmed[array_start..];
        let mut idx = 0usize;
        while let Some(name_pos) = json_segment[idx..].find("\"name\"") {
            let start = idx + name_pos;
            let Some(name) = extract_json_string_value(json_segment, start) else {
                idx = start + 6;
                continue;
            };
            let version = extract_json_string_value_after_key(json_segment, start, "\"version\"");
            if let Some(version) = version {
                candidates.push(SearchCandidate {
                    backend: Backend::Npm,
                    label: name.clone(),
                    install_id: name,
                    version: Some(version),
                    source: Some("npm".to_string()),
                });
            }
            idx = start + 6;
        }
        return dedupe_candidates(candidates);
    }

    for line in trimmed.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let name = extract_json_string_value_after_key(line, 0, "\"name\"");
        let version = extract_json_string_value_after_key(line, 0, "\"version\"");
        if let (Some(name), Some(version)) = (name, version) {
            candidates.push(SearchCandidate {
                backend: Backend::Npm,
                label: name.clone(),
                install_id: name,
                version: Some(version),
                source: Some("npm".to_string()),
            });
        }
    }

    dedupe_candidates(candidates)
}

fn parse_pip_search_candidates(output: &str) -> Vec<SearchCandidate> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|name| SearchCandidate {
            backend: Backend::Pip,
            label: name.to_string(),
            install_id: name.to_string(),
            version: None,
            source: Some("pypi".to_string()),
        })
        .collect()
}

fn parse_winget_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_winget_tabular_entries(output, false)
}

fn parse_winget_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_winget_tabular_entries(output, true)
}

fn parse_winget_tabular_entries(output: &str, upgradable: bool) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    let mut previous_line = "";
    let mut dashed_header_seen = false;
    let mut id_index = None;
    let mut version_index = None;
    let mut available_index = None;
    let mut source_index = None;

    for line in output.lines() {
        if !dashed_header_seen && line.contains("---") {
            id_index = find_char_index(previous_line, "Id");
            version_index = find_char_index(previous_line, "Version");
            available_index = find_char_index(previous_line, "Available");
            source_index = find_char_index(previous_line, "Source");
            dashed_header_seen = true;
            continue;
        }

        if dashed_header_seen {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let (Some(id_index), Some(version_index)) = (id_index, version_index) {
                let line_end = line.chars().count();
                let name = slice_chars(line, 0, id_index).trim().to_string();
                let id_end = version_index;
                let id = slice_chars(line, id_index, id_end).trim().to_string();
                let version_end = available_index.or(source_index).unwrap_or(line_end);
                let current_version = slice_chars(line, version_index, version_end)
                    .trim()
                    .to_string();
                let available_version = available_index
                    .map(|start| {
                        slice_chars(line, start, source_index.unwrap_or(line_end))
                            .trim()
                            .to_string()
                    })
                    .filter(|value| !value.is_empty());

                if !id.is_empty() {
                    entries.push(PackageListEntry {
                        backend: Backend::Winget,
                        name: if name.is_empty() { id } else { name },
                        current_version: if current_version.is_empty() {
                            "-".to_string()
                        } else {
                            current_version
                        },
                        available_version: if upgradable { available_version } else { None },
                    });
                }
            }
        }

        previous_line = line;
    }

    if dashed_header_seen {
        Some(entries)
    } else if output
        .to_ascii_lowercase()
        .contains("no installed package found")
        || output
            .to_ascii_lowercase()
            .contains("no available upgrade found")
    {
        Some(Vec::new())
    } else {
        None
    }
}

fn parse_scoop_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Installed apps")
            || trimmed.starts_with("Name")
            || trimmed.starts_with("---")
        {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        let Some(version) = parts.last() else {
            continue;
        };
        if parts.len() < 2 || !looks_like_version(version) {
            return None;
        }

        entries.push(PackageListEntry {
            backend: Backend::Scoop,
            name: parts[..parts.len() - 1].join(" "),
            current_version: normalize_list_version(version),
            available_version: None,
        });
    }
    Some(entries)
}

fn parse_scoop_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("WARN")
            || trimmed.starts_with("Held package")
            || trimmed.starts_with("Scoop was updated")
        {
            continue;
        }

        let Some((left, right)) = trimmed.split_once("->") else {
            continue;
        };
        let latest = normalize_list_version(right.trim());
        let left = left.trim().trim_end_matches(':').trim();
        let parts: Vec<&str> = left.split_whitespace().collect();
        let Some(current) = parts.last() else {
            return None;
        };
        if parts.len() < 2 {
            return None;
        }

        entries.push(PackageListEntry {
            backend: Backend::Scoop,
            name: parts[..parts.len() - 1]
                .join(" ")
                .trim_end_matches(':')
                .to_string(),
            current_version: normalize_list_version(current),
            available_version: Some(latest),
        });
    }
    Some(entries)
}

fn parse_choco_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Chocolatey v")
            || trimmed.ends_with("packages installed.")
        {
            continue;
        }

        let Some((name, version)) = trimmed.split_once('|') else {
            return None;
        };
        entries.push(PackageListEntry {
            backend: Backend::Chocolatey,
            name: name.trim().to_string(),
            current_version: normalize_list_version(version.trim()),
            available_version: None,
        });
    }
    Some(entries)
}

fn parse_choco_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Chocolatey v")
            || trimmed.ends_with("packages")
            || trimmed.ends_with("packages upgraded.")
        {
            continue;
        }

        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() < 3 {
            return None;
        }

        entries.push(PackageListEntry {
            backend: Backend::Chocolatey,
            name: parts[0].trim().to_string(),
            current_version: normalize_list_version(parts[1].trim()),
            available_version: Some(normalize_list_version(parts[2].trim())),
        });
    }
    Some(entries)
}

fn parse_npm_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Some(Vec::new());
    }
    if !trimmed.starts_with('{') {
        return None;
    }

    let mut entries = Vec::new();
    let mut in_dependencies = false;
    let mut current_name: Option<String> = None;

    for line in trimmed.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"dependencies\"") {
            in_dependencies = true;
            continue;
        }
        if !in_dependencies {
            continue;
        }
        if current_name.is_none() {
            if trimmed == "}" || trimmed == "}," {
                continue;
            }
            if trimmed.ends_with('{') && trimmed.starts_with('"') {
                let (name, _) = trimmed.split_once(':')?;
                current_name = Some(name.trim().trim_matches('"').to_string());
            }
            continue;
        }

        if trimmed.starts_with("\"version\"") {
            let version = extract_json_string_value_after_key(trimmed, 0, "\"version\"")?;
            entries.push(PackageListEntry {
                backend: Backend::Npm,
                name: current_name.take()?,
                current_version: version,
                available_version: None,
            });
        }
    }

    Some(entries)
}

fn parse_npm_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Some(Vec::new());
    }
    if !trimmed.starts_with('{') {
        return None;
    }

    let mut entries = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_version: Option<String> = None;
    let mut latest_version: Option<String> = None;

    for line in trimmed.lines() {
        let trimmed = line.trim();
        if current_name.is_none() {
            if trimmed == "{" || trimmed == "}" {
                continue;
            }
            if trimmed.ends_with('{') && trimmed.starts_with('"') {
                let (name, _) = trimmed.split_once(':')?;
                current_name = Some(name.trim().trim_matches('"').to_string());
                current_version = None;
                latest_version = None;
            }
            continue;
        }

        if trimmed.starts_with("\"current\"") {
            current_version = extract_json_string_value_after_key(trimmed, 0, "\"current\"");
        } else if trimmed.starts_with("\"latest\"") {
            latest_version = extract_json_string_value_after_key(trimmed, 0, "\"latest\"");
        } else if trimmed == "}," || trimmed == "}" {
            let name = current_name.take()?;
            entries.push(PackageListEntry {
                backend: Backend::Npm,
                name,
                current_version: current_version.take().unwrap_or_else(|| "-".to_string()),
                available_version: latest_version.take(),
            });
        }
    }

    Some(entries)
}

fn parse_pip_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_pip_json_entries(output, false)
}

fn parse_pip_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_pip_json_entries(output, true)
}

fn parse_pip_json_entries(output: &str, upgradable: bool) -> Option<Vec<PackageListEntry>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    if !trimmed.starts_with('[') {
        return None;
    }

    let mut entries = Vec::new();
    let mut idx = 0usize;
    while let Some(name_pos) = trimmed[idx..].find("\"name\"") {
        let start = idx + name_pos;
        let name = extract_json_string_value(trimmed, start)?;
        let version = extract_json_string_value_after_key(trimmed, start, "\"version\"")?;
        let latest = if upgradable {
            extract_json_string_value_after_key(trimmed, start, "\"latest_version\"")
        } else {
            None
        };
        entries.push(PackageListEntry {
            backend: Backend::Pip,
            name,
            current_version: version,
            available_version: latest,
        });
        idx = start + 6;
    }

    Some(entries)
}

fn parse_npm_show_details(output: &str) -> Option<PackageDetails> {
    let mut non_empty_lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let header = non_empty_lines.next()?;
    let (name, version) = parse_npm_show_header(header)?;
    let summary = non_empty_lines
        .next()
        .filter(|line| !looks_like_url(line))
        .map(str::to_string);

    let mut homepage = None;
    let mut license = parse_npm_header_license(header);
    let mut author = None;
    let repository = None;
    let mut keywords = Vec::new();
    let mut dependencies = Vec::new();
    let mut extra_fields = Vec::new();
    let mut section: Option<&str> = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line == header || summary.as_deref() == Some(line) {
            continue;
        }
        if homepage.is_none() && looks_like_url(line) {
            homepage = Some(line.to_string());
            continue;
        }
        if let Some(label) = line.strip_suffix(':') {
            section = Some(label);
            continue;
        }
        match section {
            Some("dependencies") => {
                for segment in line.split(',') {
                    if let Some((dependency, _)) = segment.split_once(':') {
                        let dependency = dependency.trim().to_string();
                        if !dependency.is_empty() {
                            dependencies.push(dependency);
                        }
                    }
                }
                continue;
            }
            Some("maintainers") => {
                if author.is_none() {
                    author = Some(line.trim_start_matches('-').trim().to_string());
                }
                continue;
            }
            _ => {}
        }
        section = None;
        if let Some(value) = line.strip_prefix("published ") {
            let published = value.trim().to_string();
            if author.is_none() {
                author = published
                    .split_once(" by ")
                    .map(|(_, author)| author.trim().to_string());
            }
            extra_fields.push(("Published".to_string(), published));
            continue;
        }
        if let Some((label, value)) = line.split_once(':') {
            let label = normalize_field_label(label);
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match label.as_str() {
                "Keywords" => {
                    keywords.extend(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|item| !item.is_empty())
                            .map(str::to_string),
                    );
                }
                "Maintainers" => extra_fields.push((label, value)),
                "Published" => extra_fields.push((label, value)),
                "License" if license.is_none() => license = Some(value),
                _ => {}
            }
        }
    }

    Some(PackageDetails {
        backend: Backend::Npm,
        name,
        version,
        summary,
        homepage,
        license,
        author,
        repository,
        keywords,
        dependencies,
        extra_fields,
    })
}

fn parse_npm_show_header(header: &str) -> Option<(String, String)> {
    let package_part = header.split('|').next()?.trim();
    let split_index = package_part.rfind('@')?;
    if split_index == 0 {
        return None;
    }
    Some((
        package_part[..split_index].trim().to_string(),
        package_part[split_index + 1..].trim().to_string(),
    ))
}

fn parse_npm_header_license(header: &str) -> Option<String> {
    header
        .split('|')
        .skip(1)
        .map(str::trim)
        .find(|segment| !segment.contains(':') && !segment.is_empty())
        .map(str::to_string)
}

fn parse_pip_show_details(output: &str) -> Option<PackageDetails> {
    let fields = parse_colon_fields(output);
    let name = find_field_value(&fields, &["Name"])?;
    let version = find_field_value(&fields, &["Version"])?;
    let summary = find_field_value(&fields, &["Summary"]);
    let homepage = find_field_value(&fields, &["Home-page", "Homepage"]);
    let license = find_field_value(&fields, &["License-Expression", "License"]);
    let author = find_field_value(&fields, &["Author"])
        .filter(|value| !value.trim().is_empty())
        .or_else(|| find_field_value(&fields, &["Author-email", "Author Email"]));
    let repository = None;
    let keywords = Vec::new();
    let dependencies = find_field_value(&fields, &["Requires"])
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let extra_fields = fields
        .into_iter()
        .filter(|(label, _)| {
            !matches!(
                label.as_str(),
                "Name"
                    | "Version"
                    | "Summary"
                    | "Home-page"
                    | "Homepage"
                    | "License"
                    | "License-Expression"
                    | "Author"
                    | "Author-email"
                    | "Author Email"
                    | "Requires"
            )
        })
        .map(|(label, value)| (normalize_field_label(&label), value))
        .collect();

    Some(PackageDetails {
        backend: Backend::Pip,
        name,
        version,
        summary,
        homepage,
        license,
        author,
        repository,
        keywords,
        dependencies,
        extra_fields,
    })
}

fn parse_key_value_show_details(output: &str) -> Option<PackageDetails> {
    let fields = parse_colon_fields(output);
    let name = find_field_value(&fields, &["Name", "Package Identifier", "Id", "Package"])?;
    let version = find_field_value(&fields, &["Version", "Installed Version", "Latest Version"])
        .unwrap_or_else(|| "-".to_string());
    let summary = find_field_value(
        &fields,
        &["Summary", "Description", "Short Description", "Moniker"],
    );
    let homepage = find_field_value(
        &fields,
        &["Homepage", "Home-page", "Project URL", "Publisher URL"],
    );
    let license = find_field_value(&fields, &["License", "License Url"]);
    let author = find_field_value(&fields, &["Author", "Publisher", "Maintainer"]);
    let repository = find_field_value(&fields, &["Repository", "Repository Url"]);
    let keywords = find_field_value(&fields, &["Tags", "Keywords"])
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dependencies = find_field_value(&fields, &["Dependencies", "Requires"])
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let extra_fields = fields
        .into_iter()
        .filter(|(label, _)| {
            !matches!(
                label.as_str(),
                "Name"
                    | "Package Identifier"
                    | "Id"
                    | "Package"
                    | "Version"
                    | "Installed Version"
                    | "Latest Version"
                    | "Summary"
                    | "Description"
                    | "Short Description"
                    | "Moniker"
                    | "Homepage"
                    | "Home-page"
                    | "Project URL"
                    | "Publisher URL"
                    | "License"
                    | "License Url"
                    | "Author"
                    | "Publisher"
                    | "Maintainer"
                    | "Repository"
                    | "Repository Url"
                    | "Tags"
                    | "Keywords"
                    | "Dependencies"
                    | "Requires"
            )
        })
        .map(|(label, value)| (normalize_field_label(&label), value))
        .collect();

    Some(PackageDetails {
        backend: Backend::Winget,
        name,
        version,
        summary,
        homepage,
        license,
        author,
        repository,
        keywords,
        dependencies,
        extra_fields,
    })
}

fn parse_colon_fields(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (label, value) = line.split_once(':')?;
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some((label.trim().to_string(), value.to_string()))
            }
        })
        .collect()
}

fn find_field_value(fields: &[(String, String)], names: &[&str]) -> Option<String> {
    fields
        .iter()
        .find(|(label, _)| names.iter().any(|name| label.eq_ignore_ascii_case(name)))
        .map(|(_, value)| value.clone())
}

fn normalize_field_label(label: &str) -> String {
    label
        .trim()
        .split(|ch: char| ch == '-' || ch == '_' || ch.is_whitespace())
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    let mut rendered = String::new();
                    rendered.push(first.to_ascii_uppercase());
                    rendered.push_str(&chars.as_str().to_ascii_lowercase());
                    rendered
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_pip_outdated_json_names(output: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut idx = 0usize;
    while let Some(name_pos) = output[idx..].find("\"name\"") {
        let start = idx + name_pos;
        if let Some(name) = extract_json_string_value(output, start) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        idx = start + 6;
    }
    names
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

    if label == query || install_id == query {
        0
    } else if label.starts_with(query) || install_id.starts_with(query) {
        1
    } else if label.contains(query) || install_id.contains(query) {
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

fn escape_json_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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

fn print_help() {
    let config_hint = default_config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<system-dependent>".to_string());

    println!(
        "\
{APP_NAME} {APP_VERSION}

Usage:
  {APP_NAME} [--backend <winget|scoop|choco|npm|pip>] [--config <path>] [--dry-run] [--json] [-y] <command> [args...]

Commands:
  update                   Refresh package metadata where supported
  upgrade [pkg...]         Upgrade all packages, or only the named packages
  install <pkg...>         Install one or more packages
  install --pick <query>   Search interactively across the selected/auto backends, then choose packages
  remove <pkg...>          Uninstall one or more packages
  hold [--off] <pkg...>    Add or remove an upgrade hold
  search <query...>        Search the selected backend, or all enabled+available backends in auto mode
  list [--upgradable]      List packages in a normalized table, using the selected backend or all enabled+available backends in auto mode
  show <pkg>               Show normalized package details from the selected backend, or probe all enabled+available backends in auto mode
  backends                 Show detected backend availability and enabled state
  backend list             Same as `backends`
  backend enable <name>    Enable a backend in config
  backend disable <name>   Disable a backend in config
  backend install <name>   Run a bootstrap install command when available
  backend default <name>   Set the default backend and enable it if needed
  backend default auto     Clear the explicit default and return to auto detection

Options:
  -b, --backend <name>     Select backend explicitly
      --config <path>      Load config from a custom path
      --dry-run            Print backend commands without executing them
      --json               Emit machine-readable JSON for backend management and `show` output
  -y, --yes                Assume yes where the backend supports it
  -h, --help               Show this help text
  -V, --version            Show version

Default config path:
  {config_hint}

Examples:
  {APP_NAME} update
  {APP_NAME} upgrade
  {APP_NAME} install Git.Git
  {APP_NAME} install --pick git
  {APP_NAME} hold --off Git.Git
  {APP_NAME} search powertoys
  {APP_NAME} list --upgradable
  {APP_NAME} show Git.Git
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
mod tests {
    use super::*;

    #[test]
    fn parses_hold_disable() {
        let cli = Cli::parse(["hold", "--off", "Git.Git"].into_iter().map(str::to_string))
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Subcommand::Hold {
                packages: vec!["Git.Git".to_string()],
                enable: false
            }
        );
    }

    #[test]
    fn parses_search_query() {
        let cli = Cli::parse(["search", "power", "toys"].into_iter().map(str::to_string))
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Subcommand::Search {
                query: "power toys".to_string()
            }
        );
    }

    #[test]
    fn parses_global_backend() {
        let cli = Cli::parse(
            ["--backend", "scoop", "install", "git"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("cli should parse");

        assert_eq!(cli.backend, Some(Backend::Scoop));
        assert_eq!(
            cli.command,
            Subcommand::Install {
                mode: InstallMode::Packages(vec!["git".to_string()])
            }
        );
    }

    #[test]
    fn parses_backend_enable_command() {
        let cli = Cli::parse(["backend", "enable", "pip"].into_iter().map(str::to_string))
            .expect("cli should parse");

        assert_eq!(
            cli.command,
            Subcommand::Backend {
                action: BackendAction::Enable {
                    backend: Backend::Pip,
                }
            }
        );
    }

    #[test]
    fn parses_backend_install_with_enable_flag() {
        let cli = Cli::parse(
            ["backend", "install", "npm", "--enable"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Subcommand::Backend {
                action: BackendAction::Install {
                    backend: Backend::Npm,
                    enable: true,
                }
            }
        );
    }

    #[test]
    fn parses_backend_default_auto() {
        let cli = Cli::parse(
            ["--json", "backend", "default", "auto"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("cli should parse");

        assert!(cli.json);
        assert_eq!(
            cli.command,
            Subcommand::Backend {
                action: BackendAction::Default { backend: None }
            }
        );
    }

    #[test]
    fn parses_json_show_command() {
        let cli = Cli::parse(
            ["--json", "show", "requests"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("cli should parse");

        assert!(cli.json);
        assert_eq!(
            cli.command,
            Subcommand::Show {
                package: "requests".to_string()
            }
        );
    }

    #[test]
    fn parses_interactive_install() {
        let cli = Cli::parse(
            ["install", "--pick", "visual", "studio"]
                .into_iter()
                .map(str::to_string),
        )
        .expect("cli should parse");

        assert_eq!(
            cli.command,
            Subcommand::Install {
                mode: InstallMode::Pick("visual studio".to_string())
            }
        );
    }

    #[test]
    fn parses_config_file() {
        let config = Config::parse(
            r#"
backend = "winget"
assume_yes = true
winget_source = "winget"
choco_source = "https://community.chocolatey.org/api/v2/"
scoop_bucket = "extras"
pip_user = true
"#,
        )
        .expect("config should parse");

        assert_eq!(config.backend, Some(Backend::Winget));
        assert!(config.assume_yes);
        assert_eq!(config.winget_source(), Some("winget"));
        assert_eq!(
            config.choco_source(),
            Some("https://community.chocolatey.org/api/v2/")
        );
        assert_eq!(config.qualify_scoop_package("git"), "extras/git");
        assert!(config.pip_user);
        assert!(config.enable_winget);
        assert!(config.enable_pip);
    }

    #[test]
    fn winget_upgrade_all_plan_matches_expectation() {
        let config = Config::default();
        let runtime = RuntimeSettings {
            assume_yes: false,
            config: &config,
        };
        let plan = Backend::Winget
            .plan(&Subcommand::Upgrade { packages: vec![] }, &runtime)
            .expect("plan should build");

        assert_eq!(
            plan,
            vec![Invocation::owned(
                "winget",
                vec![
                    "upgrade".to_string(),
                    "--all".to_string(),
                    "--include-unknown".to_string(),
                    "--accept-source-agreements".to_string(),
                    "--accept-package-agreements".to_string(),
                ],
            )]
        );
    }

    #[test]
    fn winget_search_uses_configured_source() {
        let config = Config {
            winget_source: Some("winget".to_string()),
            ..Config::default()
        };
        let runtime = RuntimeSettings {
            assume_yes: false,
            config: &config,
        };
        let plan = Backend::Winget
            .plan(
                &Subcommand::Search {
                    query: "git".to_string(),
                },
                &runtime,
            )
            .expect("plan should build");

        assert_eq!(
            plan,
            vec![Invocation::owned(
                "winget",
                vec![
                    "search".to_string(),
                    "git".to_string(),
                    "--source".to_string(),
                    "winget".to_string(),
                ],
            )]
        );
    }

    #[test]
    fn scoop_install_uses_bucket_prefix() {
        let config = Config {
            scoop_bucket: Some("extras".to_string()),
            ..Config::default()
        };
        let runtime = RuntimeSettings {
            assume_yes: false,
            config: &config,
        };
        let plan = Backend::Scoop
            .plan(
                &Subcommand::Install {
                    mode: InstallMode::Packages(vec!["git".to_string()]),
                },
                &runtime,
            )
            .expect("plan should build");

        assert_eq!(
            plan,
            vec![Invocation::owned(
                "scoop",
                vec!["install".to_string(), "extras/git".to_string()],
            )]
        );
    }

    #[test]
    fn chocolatey_hold_plan_uses_pin_add() {
        let config = Config::default();
        let runtime = RuntimeSettings {
            assume_yes: false,
            config: &config,
        };
        let plan = Backend::Chocolatey
            .plan(
                &Subcommand::Hold {
                    packages: vec!["git".to_string()],
                    enable: true,
                },
                &runtime,
            )
            .expect("plan should build");

        assert_eq!(
            plan,
            vec![Invocation::owned(
                "choco",
                vec![
                    "pin".to_string(),
                    "add".to_string(),
                    "--name".to_string(),
                    "git".to_string(),
                ],
            )]
        );
    }

    #[test]
    fn parses_selection_ranges_and_deduplicates() {
        let selected = parse_selection("1 3-4 4,2", 5).expect("selection should parse");
        assert_eq!(selected, vec![1, 3, 4, 2]);
    }

    #[test]
    fn parses_scoop_search_candidates_from_bucket_sections() {
        let candidates = Backend::Scoop.parse_search_candidates(
            "'main' bucket:\n7zip 24.09\nWARN ignored\n'versions' bucket:\npython310 3.10.11\nNo Matches Found\n",
        );

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Scoop,
                    label: "7zip".to_string(),
                    install_id: "main/7zip".to_string(),
                    version: Some("24.09".to_string()),
                    source: Some("main".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Scoop,
                    label: "python310".to_string(),
                    install_id: "versions/python310".to_string(),
                    version: Some("3.10.11".to_string()),
                    source: Some("versions".to_string()),
                }
            ]
        );
    }

    #[test]
    fn parses_choco_search_candidates_with_limit_output() {
        let candidates =
            Backend::Chocolatey.parse_search_candidates("git|2.48.1\nripgrep|14.1.1\n");

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Chocolatey,
                    label: "git".to_string(),
                    install_id: "git".to_string(),
                    version: Some("2.48.1".to_string()),
                    source: None,
                },
                SearchCandidate {
                    backend: Backend::Chocolatey,
                    label: "ripgrep".to_string(),
                    install_id: "ripgrep".to_string(),
                    version: Some("14.1.1".to_string()),
                    source: None,
                }
            ]
        );
    }

    #[test]
    fn parses_winget_search_candidates_from_table_output() {
        let output = "\
Name                         Id                           Version Source\n\
-----------------------------------------------------------------------\n\
Git                          Git.Git                      2.47.1  winget\n\
Microsoft PowerToys          Microsoft.PowerToys          0.90.1  winget\n";

        let candidates = Backend::Winget.parse_search_candidates(output);

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Winget,
                    label: "Git".to_string(),
                    install_id: "Git.Git".to_string(),
                    version: Some("2.47.1".to_string()),
                    source: Some("winget".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Winget,
                    label: "Microsoft PowerToys".to_string(),
                    install_id: "Microsoft.PowerToys".to_string(),
                    version: Some("0.90.1".to_string()),
                    source: Some("winget".to_string()),
                }
            ]
        );
    }

    #[test]
    fn parses_npm_search_candidates_from_warning_prefixed_array() {
        let output = r#"npm warn config global
[
  { "name": "left-pad", "version": "1.3.0" },
  { "name": "@types/node", "version": "24.0.0" }
]"#;

        let candidates = Backend::Npm.parse_search_candidates(output);

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Npm,
                    label: "left-pad".to_string(),
                    install_id: "left-pad".to_string(),
                    version: Some("1.3.0".to_string()),
                    source: Some("npm".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Npm,
                    label: "@types/node".to_string(),
                    install_id: "@types/node".to_string(),
                    version: Some("24.0.0".to_string()),
                    source: Some("npm".to_string()),
                }
            ]
        );
    }

    #[test]
    fn parses_pip_search_candidates_from_python_script_output() {
        let candidates = Backend::Pip.parse_search_candidates("requests\nrequests-cache\n");

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Pip,
                    label: "requests".to_string(),
                    install_id: "requests".to_string(),
                    version: None,
                    source: Some("pypi".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Pip,
                    label: "requests-cache".to_string(),
                    install_id: "requests-cache".to_string(),
                    version: None,
                    source: Some("pypi".to_string()),
                }
            ]
        );
    }

    #[test]
    fn parses_npm_installed_list_from_json() {
        let output = r#"{
  "name": "lib",
  "dependencies": {
    "@openai/codex": {
      "version": "0.121.0",
      "overridden": false
    },
    "npm": {
      "version": "11.12.1",
      "overridden": false
    }
  }
}"#;

        assert_eq!(
            Backend::Npm.parse_list_entries(false, output),
            Some(vec![
                PackageListEntry {
                    backend: Backend::Npm,
                    name: "@openai/codex".to_string(),
                    current_version: "0.121.0".to_string(),
                    available_version: None,
                },
                PackageListEntry {
                    backend: Backend::Npm,
                    name: "npm".to_string(),
                    current_version: "11.12.1".to_string(),
                    available_version: None,
                }
            ])
        );
    }

    #[test]
    fn parses_npm_upgradable_list_from_json() {
        let output = r#"{
  "@openai/codex": {
    "current": "0.118.0",
    "wanted": "0.121.0",
    "latest": "0.121.0"
  },
  "happy-coder": {
    "current": "0.13.0",
    "wanted": "0.13.1",
    "latest": "0.13.1"
  }
}"#;

        assert_eq!(
            Backend::Npm.parse_list_entries(true, output),
            Some(vec![
                PackageListEntry {
                    backend: Backend::Npm,
                    name: "@openai/codex".to_string(),
                    current_version: "0.118.0".to_string(),
                    available_version: Some("0.121.0".to_string()),
                },
                PackageListEntry {
                    backend: Backend::Npm,
                    name: "happy-coder".to_string(),
                    current_version: "0.13.0".to_string(),
                    available_version: Some("0.13.1".to_string()),
                }
            ])
        );
    }

    #[test]
    fn parses_pip_installed_list_from_json() {
        let output = r#"[
  {"name": "certifi", "version": "2026.2.25"},
  {"name": "pip", "version": "26.0"}
]"#;

        assert_eq!(
            Backend::Pip.parse_list_entries(false, output),
            Some(vec![
                PackageListEntry {
                    backend: Backend::Pip,
                    name: "certifi".to_string(),
                    current_version: "2026.2.25".to_string(),
                    available_version: None,
                },
                PackageListEntry {
                    backend: Backend::Pip,
                    name: "pip".to_string(),
                    current_version: "26.0".to_string(),
                    available_version: None,
                }
            ])
        );
    }

    #[test]
    fn parses_pip_upgradable_list_from_json() {
        let output = r#"[
  {
    "name": "pip",
    "version": "26.0",
    "latest_version": "26.0.1",
    "latest_filetype": "wheel"
  }
]"#;

        assert_eq!(
            Backend::Pip.parse_list_entries(true, output),
            Some(vec![PackageListEntry {
                backend: Backend::Pip,
                name: "pip".to_string(),
                current_version: "26.0".to_string(),
                available_version: Some("26.0.1".to_string()),
            }])
        );
    }

    #[test]
    fn parses_pip_show_details_from_key_value_output() {
        let output = "\
Name: pip
Version: 26.0
Summary: The PyPA recommended tool for installing Python packages.
Home-page: https://pip.pypa.io/
Author-email: The pip developers <distutils-sig@python.org>
License-Expression: MIT
Location: /opt/homebrew/lib/python3.14/site-packages
Requires: setuptools, wheel
Required-by:
";

        assert_eq!(
            Backend::Pip.parse_show_details(output),
            Some(PackageDetails {
                backend: Backend::Pip,
                name: "pip".to_string(),
                version: "26.0".to_string(),
                summary: Some(
                    "The PyPA recommended tool for installing Python packages.".to_string()
                ),
                homepage: Some("https://pip.pypa.io/".to_string()),
                license: Some("MIT".to_string()),
                author: Some("The pip developers <distutils-sig@python.org>".to_string()),
                repository: None,
                keywords: vec![],
                dependencies: vec!["setuptools".to_string(), "wheel".to_string()],
                extra_fields: vec![(
                    "Location".to_string(),
                    "/opt/homebrew/lib/python3.14/site-packages".to_string(),
                )],
            })
        );
    }

    #[test]
    fn parses_npm_show_details_from_plain_output() {
        let output = "\
requests@0.3.0 | MIT | deps: 7 | versions: 13
An streaming XHR abstraction that works in browsers and node.js
https://github.com/unshiftio/requests

keywords: request, xhr, requests
published over a year ago by swaagie <martijn@swaagman.online>
";

        assert_eq!(
            Backend::Npm.parse_show_details(output),
            Some(PackageDetails {
                backend: Backend::Npm,
                name: "requests".to_string(),
                version: "0.3.0".to_string(),
                summary: Some(
                    "An streaming XHR abstraction that works in browsers and node.js".to_string()
                ),
                homepage: Some("https://github.com/unshiftio/requests".to_string()),
                license: Some("MIT".to_string()),
                author: Some("swaagie <martijn@swaagman.online>".to_string()),
                repository: None,
                keywords: vec![
                    "request".to_string(),
                    "xhr".to_string(),
                    "requests".to_string(),
                ],
                dependencies: vec![],
                extra_fields: vec![(
                    "Published".to_string(),
                    "over a year ago by swaagie <martijn@swaagman.online>".to_string(),
                )],
            })
        );
    }

    #[test]
    fn dedupes_search_candidates_per_backend() {
        let candidates = dedupe_search_candidates(vec![
            SearchCandidate {
                backend: Backend::Npm,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: Some("1.0.0".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Npm,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: Some("1.0.1".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Pip,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            },
        ]);

        assert_eq!(
            candidates,
            vec![
                SearchCandidate {
                    backend: Backend::Npm,
                    label: "requests".to_string(),
                    install_id: "requests".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: Some("npm".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Pip,
                    label: "requests".to_string(),
                    install_id: "requests".to_string(),
                    version: None,
                    source: Some("pypi".to_string()),
                }
            ]
        );
    }

    #[test]
    fn sorts_search_candidates_by_match_quality_then_backend_order() {
        let candidates = sort_search_candidates(
            vec![
                SearchCandidate {
                    backend: Backend::Pip,
                    label: "requests".to_string(),
                    install_id: "requests".to_string(),
                    version: None,
                    source: Some("pypi".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Npm,
                    label: "requests".to_string(),
                    install_id: "requests".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: Some("npm".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Npm,
                    label: "requests-cache".to_string(),
                    install_id: "requests-cache".to_string(),
                    version: Some("1.0.0".to_string()),
                    source: Some("npm".to_string()),
                },
                SearchCandidate {
                    backend: Backend::Pip,
                    label: "python-requests-tools".to_string(),
                    install_id: "python-requests-tools".to_string(),
                    version: None,
                    source: Some("pypi".to_string()),
                },
            ],
            "requests",
            &[Backend::Npm, Backend::Pip],
        );

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| format!("{}:{}", candidate.backend, candidate.install_id))
                .collect::<Vec<_>>(),
            vec![
                "npm:requests".to_string(),
                "pip:requests".to_string(),
                "npm:requests-cache".to_string(),
                "pip:python-requests-tools".to_string(),
            ]
        );
    }

    #[test]
    fn renders_backend_output_section_with_header() {
        assert_eq!(
            render_backend_output_section(Backend::Pip, "\nName: requests\nVersion: 2.32.0\n"),
            Some("== pip ==\nName: requests\nVersion: 2.32.0".to_string())
        );
    }

    #[test]
    fn renders_multi_backend_package_details_as_comparison() {
        let rendered = render_package_details_sections(&[
            PackageDetails {
                backend: Backend::Npm,
                name: "pip".to_string(),
                version: "0.0.1".to_string(),
                summary: Some("Freckle CLI tool using node.js".to_string()),
                homepage: None,
                license: Some("Proprietary".to_string()),
                author: Some("sirkitree <sirkitree@gmail.com>".to_string()),
                repository: None,
                keywords: vec![],
                dependencies: vec!["optimist".to_string(), "freckle".to_string()],
                extra_fields: vec![(
                    "Published".to_string(),
                    "over a year ago by sirkitree <sirkitree@gmail.com>".to_string(),
                )],
            },
            PackageDetails {
                backend: Backend::Pip,
                name: "pip".to_string(),
                version: "26.0".to_string(),
                summary: Some(
                    "The PyPA recommended tool for installing Python packages.".to_string(),
                ),
                homepage: Some("https://pip.pypa.io/".to_string()),
                license: Some("MIT".to_string()),
                author: Some("The pip developers <distutils-sig@python.org>".to_string()),
                repository: None,
                keywords: vec![],
                dependencies: vec![],
                extra_fields: vec![(
                    "Location".to_string(),
                    "/opt/homebrew/lib/python3.14/site-packages".to_string(),
                )],
            },
        ]);

        assert_eq!(
            rendered,
            "\
== comparison ==
Name      : pip
Version   :
  npm      0.0.1
  pip      26.0
Summary   :
  npm      Freckle CLI tool using node.js
  pip      The PyPA recommended tool for installing Python packages.
Homepage  : https://pip.pypa.io/
License   :
  npm      Proprietary
  pip      MIT
Author    :
  npm      sirkitree <sirkitree@gmail.com>
  pip      The pip developers <distutils-sig@python.org>
Depends On: optimist, freckle

== npm extras ==
Published : over a year ago by sirkitree <sirkitree@gmail.com>

== pip extras ==
Location  : /opt/homebrew/lib/python3.14/site-packages"
                .to_string()
        );
    }

    #[test]
    fn render_command_failure_prefers_stderr_details() {
        let invocation = Invocation::owned("pip", vec!["show".to_string(), "requests".to_string()]);
        let capture = CommandCapture {
            stdout: String::new(),
            stderr: "Package(s) not found".to_string(),
            success: false,
            status_code: 1,
        };

        assert_eq!(
            render_command_failure(&invocation, &capture),
            "backend command failed with exit code 1: pip show requests (Package(s) not found)"
                .to_string()
        );
    }

    #[test]
    fn npm_upgradable_list_accepts_exit_code_one() {
        let capture = CommandCapture {
            stdout: "{\"typescript\":{}}".to_string(),
            stderr: String::new(),
            success: false,
            status_code: 1,
        };

        assert!(Backend::Npm.accepts_list_capture(true, &capture));
        assert!(!Backend::Npm.accepts_list_capture(false, &capture));
        assert!(!Backend::Pip.accepts_list_capture(true, &capture));
    }

    #[test]
    fn pip_hold_returns_informational_message() {
        let config = Config::default();
        let runtime = RuntimeSettings {
            assume_yes: false,
            config: &config,
        };
        let plan = Backend::Pip
            .plan(
                &Subcommand::Hold {
                    packages: vec!["requests".to_string()],
                    enable: true,
                },
                &runtime,
            )
            .expect("plan should build");

        assert_eq!(
            plan,
            vec![Invocation::message(
                "This backend does not support a native hold/pin operation.",
            )]
        );
    }

    #[test]
    fn disabling_backend_clears_default_selection() {
        let mut config = Config {
            backend: Some(Backend::Npm),
            ..Config::default()
        };
        config.set_backend_enabled(Backend::Npm, false);

        assert!(!config.is_backend_enabled(Backend::Npm));
        assert_eq!(config.backend, None);
    }

    #[test]
    fn backend_status_json_contains_key_fields() {
        let config = Config {
            backend: Some(Backend::Pip),
            enable_choco: false,
            ..Config::default()
        };

        let json = render_backend_statuses_json(&config);

        assert!(json.contains("\"backend\":\"pip\""));
        assert!(json.contains("\"default_selected\":true"));
        assert!(json.contains("\"backend\":\"choco\""));
        assert!(json.contains("\"enabled\":false"));
    }

    #[test]
    fn show_results_json_contains_detail_fields() {
        let json = render_show_results_json(&[ShowBackendResult {
            backend: Backend::Pip,
            command: "python3 -m pip show pip".to_string(),
            success: true,
            dry_run: false,
            details: Some(PackageDetails {
                backend: Backend::Pip,
                name: "pip".to_string(),
                version: "26.0".to_string(),
                summary: Some(
                    "The PyPA recommended tool for installing Python packages.".to_string(),
                ),
                homepage: Some("https://pip.pypa.io/".to_string()),
                license: Some("MIT".to_string()),
                author: Some("The pip developers <distutils-sig@python.org>".to_string()),
                repository: None,
                keywords: vec![],
                dependencies: vec!["setuptools".to_string()],
                extra_fields: vec![(
                    "Location".to_string(),
                    "/opt/homebrew/lib/python3.14/site-packages".to_string(),
                )],
            }),
            raw_output: None,
            error: None,
        }]);

        assert!(json.contains("\"backend\":\"pip\""));
        assert!(json.contains("\"command\":\"python3 -m pip show pip\""));
        assert!(json.contains("\"details\":{"));
        assert!(json.contains("\"name\":\"pip\""));
        assert!(json.contains("\"dependencies\":[\"setuptools\"]"));
        assert!(json.contains(
            "\"extra_fields\":{\"Location\":\"/opt/homebrew/lib/python3.14/site-packages\"}"
        ));
        assert!(json.contains("\"error\":null"));
    }
}
