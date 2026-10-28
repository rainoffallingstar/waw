//! `list` and `show`: read-only queries that aggregate across backends.

use super::super::*;

pub(crate) fn run_list(
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

pub(crate) fn run_show(
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
