//! `install`, `remove` and `upgrade`: the interactive mutating commands.
//!
//! Each one searches first, asks what to act on, then plans per backend so a
//! single invocation can cover packages coming from several tools.

use super::super::*;

pub(crate) fn run_interactive_install(
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

    let selected_indices = match choose_candidates("install", &candidates, runtime.assume_yes)? {
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
            // Interactive install has no version text: the user picked an id
            // from the search list, so nothing here can be a false pin.
            let specs: Vec<PackageSpec> = packages
                .iter()
                .map(|id| PackageSpec::without_version(id))
                .collect();
            invocations.extend(backend.plan_install(&specs, runtime));
        }
    }

    execute_invocations(invocations, dry_run, false, runtime)
}

pub(crate) fn run_interactive_remove(
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

    let selected_indices = match choose_candidates("remove", &candidates, runtime.assume_yes)? {
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

pub(crate) fn run_interactive_upgrade(
    backends: Vec<Backend>,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
    config_path: Option<&Path>,
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
            let packages = filter_held_upgrade_targets(backend, packages, config_path)?;
            if !packages.is_empty() {
                warn_on_inexact_version_pins(backend, &packages);
                invocations.extend(backend.plan_upgrade(&packages, runtime));
            }
        }
    }

    execute_invocations(invocations, dry_run, true, runtime)
}

/// Warns once per backend when a versioned request cannot be reproduced.
///
/// Only Scoop regenerates the manifest from the bucket's current definition,
/// so `gh@2.7.0` there is a version string rather than the 2.7.0 manifest.
/// Saying so is the whole fix: refusing would be hostile, and silently
/// pretending it is a pin is worse than either.
pub(crate) fn warn_on_inexact_version_pins(backend: Backend, packages: &[String]) {
    if backend.version_pinning_is_exact() {
        return;
    }
    let versioned: Vec<&str> = packages
        .iter()
        .filter(|package| PackageSpec::parse(package).version.is_some())
        .map(String::as_str)
        .collect();
    if versioned.is_empty() {
        return;
    }

    eprintln!(
        "warning: {backend} regenerates the manifest for a versioned install, \
         so {} may not match that release exactly. See `waw --help`.",
        versioned.join(", ")
    );
}
