//! `update` and `search`: the non-interactive commands.
//!
//! `update` refreshes each backend's metadata; several backends have no
//! refresh step at all, which is reported rather than pretended.

use super::super::*;

pub(crate) fn run_update_all(
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

pub(crate) fn run_search(
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
