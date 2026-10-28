//! `hold` / `hold --list`.
//!
//! `hold` is not a single backend invocation, which is why it lives here
//! rather than in `Backend::plan`:
//!
//! * winget needs a package *id* (`Git.Git`), but users type `git`.
//! * npm and pip have no pin at all, so waw records those in `holds.toml`.
//!
//! `--list` then merges both sources: native pins queried from each backend,
//! plus the waw-owned file, so one table shows every held package.

use super::super::*;

pub(crate) fn run_hold(
    backend: Backend,
    packages: &[String],
    enable: bool,
    runtime: &RuntimeSettings<'_>,
    dry_run: bool,
    config_path: Option<&Path>,
) -> Result<ExitCode, String> {
    // Hold state lives in waw's own file for backends without a native pin.
    // Everything else delegates, because a native pin is also honoured by that
    // tool's own upgrade command and by other package managers' UIs.
    if backend.uses_waw_holds() {
        return run_waw_hold(backend, packages, enable, dry_run, config_path);
    }

    let targets = if backend == Backend::Winget {
        resolve_winget_ids(packages, runtime)?
    } else {
        packages.to_vec()
    };

    if dry_run {
        // Show the resolved command, not what the user typed: the whole point
        // of resolution is that `--dry-run` stops lying about `--id git`.
        for package in &targets {
            for invocation in backend.plan_hold_pin(std::slice::from_ref(package), enable, runtime)
            {
                println!("> {}", invocation.render_for_display());
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    execute_invocations(
        backend.plan_hold_pin(&targets, enable, runtime),
        false,
        true,
        runtime,
    )
}

/// Records a hold in `holds.toml` for a backend that cannot enforce one.
fn run_waw_hold(
    backend: Backend,
    packages: &[String],
    enable: bool,
    dry_run: bool,
    config_path: Option<&Path>,
) -> Result<ExitCode, String> {
    let mut holds = Holds::load(config_path)?;

    if dry_run {
        for package in packages {
            let verb = if enable { "hold" } else { "unhold" };
            println!("would {verb} {package} for {backend} (recorded in holds.toml)");
        }
        return Ok(ExitCode::SUCCESS);
    }

    if enable {
        for package in packages {
            holds.insert(backend, package.clone());
            println!("{package} is now held for {backend}");
        }
    } else {
        for package in packages {
            if holds.remove(backend, package) {
                println!("{package} is no longer held for {backend}");
            } else {
                // Idempotent: releasing something that was not held is not an
                // error, but saying so avoids the impression that it worked.
                println!("{package} was not held for {backend}");
            }
        }
    }

    holds.save(config_path)?;
    println!(
        "Note: {backend} has no native pin, so waw skips held packages when it upgrades them."
    );
    Ok(ExitCode::SUCCESS)
}

/// Resolves user text to winget package ids.
///
/// `waw hold git` must become `--id Git.Git`; passing `git` straight through
/// looks fine under `--dry-run` and fails for real. Installed packages are
/// used as the source of truth because a pin only means something for a
/// package that is present, and they are also the set the user just saw in
/// `waw list`.
///
/// An argument that already looks like a full id and matches nothing locally
/// is passed through unchanged: winget is the authority on its own ids, and
/// refusing here would block legitimate ids that are simply not installed.
fn resolve_winget_ids(
    packages: &[String],
    runtime: &RuntimeSettings<'_>,
) -> Result<Vec<String>, String> {
    let needs_resolution = packages
        .iter()
        .any(|package| !looks_like_winget_id(package));
    if !needs_resolution {
        return Ok(packages.to_vec());
    }

    let invocation = Backend::Winget
        .plan_list(false, runtime)
        .into_iter()
        .next()
        .expect("winget list should always produce one invocation");
    let capture = run_capture_detailed_with_label(
        &invocation,
        "Resolving winget package ids",
        runtime.command_timeout,
    )?;
    if !capture.success {
        return Err(render_command_failure(&invocation, &capture));
    }
    let entries = Backend::Winget
        .parse_list_entries(false, &capture.stdout)
        .ok_or_else(|| "could not parse the winget package list".to_string())?;

    let mut resolved = Vec::new();
    for package in packages {
        if looks_like_winget_id(package) {
            resolved.push(package.clone());
            continue;
        }
        resolved.push(resolve_one_winget_id(package, &entries)?);
    }
    Ok(resolved)
}

pub(crate) fn resolve_one_winget_id(
    package: &str,
    entries: &[PackageListEntry],
) -> Result<String, String> {
    let wanted = package.trim();

    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.package_id.eq_ignore_ascii_case(wanted))
    {
        return Ok(entry.package_id.clone());
    }
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(wanted))
    {
        return Ok(entry.package_id.clone());
    }

    // `git` should still find `Git.Git`: compare the last id segment, which is
    // how winget names are conventionally built (`Publisher.Product`).
    let mut matches: Vec<&PackageListEntry> = entries
        .iter()
        .filter(|entry| {
            entry
                .package_id
                .rsplit('.')
                .next()
                .map(|segment| segment.eq_ignore_ascii_case(wanted))
                .unwrap_or(false)
        })
        .collect();

    match matches.len() {
        1 => Ok(matches.remove(0).package_id.clone()),
        0 => Err(format!(
            "winget has no installed package matching `{wanted}`. \
             Pass the full id (for example `waw hold Git.Git`)."
        )),
        _ => {
            let mut ids: Vec<String> = matches
                .into_iter()
                .map(|entry| entry.package_id.clone())
                .collect();
            ids.sort();
            Err(format!(
                "`{wanted}` is ambiguous for winget: {}. Pass the full id.",
                ids.join(", ")
            ))
        }
    }
}

/// Heuristic for "this is already an id, do not search for it".
///
/// Winget ids are dotted (`Git.Git`, `Microsoft.PowerToys`); package names are
/// not, so a dot is a strong enough signal to skip a whole listing round trip.
pub(crate) fn looks_like_winget_id(package: &str) -> bool {
    package.contains('.') && !package.contains(' ')
}

pub(crate) fn run_hold_list(
    backends: Vec<Backend>,
    runtime: &RuntimeSettings<'_>,
    config_path: Option<&Path>,
) -> Result<ExitCode, String> {
    let holds = Holds::load(config_path)?;
    let mut rows: Vec<(Backend, String, String)> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for backend in &backends {
        if backend.uses_waw_holds() {
            rows.extend(
                holds
                    .packages(*backend)
                    .iter()
                    .map(|package| (*backend, package.clone(), "waw".to_string())),
            );
            continue;
        }

        let Some(invocation) = backend.plan_hold_list() else {
            continue;
        };
        let capture = match run_capture_detailed_with_label(
            &invocation,
            &format!("Listing {backend} holds"),
            runtime.command_timeout,
        ) {
            Ok(capture) if capture.success => capture,
            Ok(capture) => {
                failures.push(format!(
                    "{backend}: {}",
                    render_command_failure(&invocation, &capture)
                ));
                continue;
            }
            Err(error) => {
                failures.push(format!("{backend}: {error}"));
                continue;
            }
        };

        rows.extend(
            backend
                .parse_holds(&capture.stdout)
                .into_iter()
                .map(|(package, pin)| (*backend, package, pin)),
        );
    }

    print_hold_rows(&rows);

    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("warning: {failure}");
        }
        return Err(format!(
            "failed to list holds for {} backend(s)",
            failures.len()
        ));
    }

    Ok(ExitCode::SUCCESS)
}

fn print_hold_rows(rows: &[(Backend, String, String)]) {
    if rows.is_empty() {
        println!("No held packages found.");
        return;
    }

    let backend_width = rows
        .iter()
        .map(|row| row.0.to_string().len())
        .max()
        .unwrap_or(7)
        .max(7);
    let package_width = rows
        .iter()
        .map(|row| row.1.chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let pin_width = rows
        .iter()
        .map(|row| row.2.chars().count())
        .max()
        .unwrap_or(8)
        .max(8);

    println!();
    println!(
        "{:<backend_width$}  {:<package_width$}  {:<pin_width$}",
        "Backend", "Package", "Pin type"
    );
    println!(
        "{:-<backend_width$}  {:-<package_width$}  {:-<pin_width$}",
        "", "", ""
    );
    for (backend, package, pin) in rows {
        println!("{backend:<backend_width$}  {package:<package_width$}  {pin:<pin_width$}");
    }
}

/// Removes waw-held packages from an upgrade target list.
///
/// Native pins do not need this: winget, scoop and choco already refuse to
/// upgrade a pinned package on their own, and re-checking here would let a
/// stale `holds.toml` override a pin the user deliberately removed with the
/// backend's own tool. Only the backends whose hold lives in that file are
/// filtered, which keeps waw from being a second, silently disagreeing source
/// of truth.
pub(crate) fn filter_held_upgrade_targets(
    backend: Backend,
    packages: Vec<String>,
    config_path: Option<&Path>,
) -> Result<Vec<String>, String> {
    if !backend.uses_waw_holds() {
        return Ok(packages);
    }

    let holds = Holds::load(config_path)?;
    let (kept, skipped) = holds.filter_upgrade_targets(backend, &packages);
    for package in &skipped {
        println!("skipping {package}: held for {backend} (see `waw hold --list`)");
    }
    Ok(kept)
}
