//! Human-readable and JSON rendering of command results.
//!
//! Kept separate from the command handlers so the output shape can be
//! reviewed without reading the execution logic.

use super::*;

pub(crate) fn print_package_details_sections(details: &[PackageDetails]) {
    let rendered = render_package_details_sections(details);
    if !rendered.is_empty() {
        println!();
        println!("{rendered}");
    }
}

pub(crate) fn render_package_details_sections(details: &[PackageDetails]) -> String {
    match details {
        [] => String::new(),
        [detail] => render_single_package_details(detail),
        _ => render_multi_package_details(details),
    }
}

pub(crate) fn render_single_package_details(detail: &PackageDetails) -> String {
    let mut lines = vec![format!("== {} ==", detail.backend)];
    append_package_detail_lines(&mut lines, detail);
    lines.join("\n")
}

pub(crate) fn render_multi_package_details(details: &[PackageDetails]) -> String {
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

pub(crate) fn append_package_detail_lines(lines: &mut Vec<String>, detail: &PackageDetails) {
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

pub(crate) fn package_detail_field_value(detail: &PackageDetails, field: &str) -> Option<String> {
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

pub(crate) fn render_show_results_json(results: &[ShowBackendResult]) -> String {
    let items = results
        .iter()
        .map(render_show_result_json)
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!("[\n  {items}\n]")
}

pub(crate) fn render_list_results_json(rows: &[PackageListEntry], upgradable: bool) -> String {
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

pub(crate) fn render_search_results_json(candidates: &[SearchCandidate]) -> String {
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

pub(crate) fn render_show_result_json(result: &ShowBackendResult) -> String {
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

pub(crate) fn render_package_details_json(details: &PackageDetails) -> String {
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

pub(crate) fn render_json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

pub(crate) fn render_json_string_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", escape_json_string(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{items}]")
}

pub(crate) fn render_json_object(entries: &[(String, String)]) -> String {
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
