//! Candidate collection: turning backend searches and listings into one
//! ordered candidate list.
//!
//! Every path here is read-only, and multi-backend cases capture in parallel
//! because the backends are independent processes.

use super::super::*;

pub(crate) fn run_captures_parallel(
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

pub(crate) fn collect_search_candidates(
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

pub(crate) fn collect_installed_candidates(
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

    if let Some(error) =
        all_backends_failed("inspect installed packages", backends.len(), &failures)
    {
        return Err(error);
    }

    let candidates = dedupe_installed_candidates(candidates);
    let candidates = sort_search_candidates(candidates, query, backends);
    Ok(candidates)
}

pub(crate) fn collect_upgradable_candidates(
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
                    eprintln!("warning: failed to inspect pending upgrades for {backend}: {error}");
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
                return Err(error.clone());
            }
        };
        if !backend.accepts_list_capture(true, capture) {
            let error = render_command_failure(&invocations[index], capture);
            if aggregate {
                eprintln!("warning: failed to inspect pending upgrades for {backend}: {error}");
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

    if let Some(error) = all_backends_failed("inspect pending upgrades", backends.len(), &failures)
    {
        return Err(error);
    }

    let candidates = dedupe_installed_candidates(candidates);
    Ok(sort_search_candidates(candidates, "", backends))
}
