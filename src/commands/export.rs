//! `export` and `import`: snapshot installed packages and replay a manifest.
//!
//! Manifests are JSON. Two shapes are accepted on import: waw's own
//! `{"packages": [...]}` and winget's `{"Sources": [{"Packages": [...]}]}`, so
//! the output of `winget export` can be fed straight back in.

use crate::json::{escape_json_string, parse_json, JsonValue};
use crate::model::*;
use crate::render::render_json_optional_string;
use crate::Backend;
use super::super::*;

/// One entry in an exported manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestPackage {
    pub(crate) backend: Backend,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: Option<String>,
}

/// Packages winget can list but cannot reinstall: ARP and MSIX entries are
/// inventory resources, not installable identifiers. On this machine 111 of
/// 205 listed packages are like this, so exporting them would produce a
/// manifest that fails on import.
pub(crate) fn is_installable_package_id(backend: Backend, package_id: &str) -> bool {
    if backend != Backend::Winget {
        return true;
    }
    !(package_id.starts_with("ARP\\") || package_id.starts_with("MSIX\\"))
}

pub(crate) fn collect_manifest_packages(
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

pub(crate) fn render_manifest_json(packages: &[ManifestPackage]) -> String {
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
pub(crate) fn run_bucket_command(
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

pub(crate) fn run_export(
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

pub(crate) fn parse_manifest(contents: &str) -> Result<Vec<ManifestPackage>, String> {
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

pub(crate) fn run_import(
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
