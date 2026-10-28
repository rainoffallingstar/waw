//! Output parsers for every backend's textual output.
//!
//! Each backend prints its own table or key/value format; these functions
//! normalise that text into the shared data types. Localised Windows output
//! (Chinese field labels, full-width colons) is handled here as well.

use super::*;

pub(crate) fn parse_winget_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let mut candidates = Vec::new();
    let mut dashed_header_seen = false;

    for line in output.lines() {
        if !dashed_header_seen && line.contains("---") {
            dashed_header_seen = true;
            continue;
        }

        if dashed_header_seen {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(candidate) = parse_winget_search_candidate_line(trimmed) {
                candidates.push(candidate);
            }
        }
    }

    candidates
}

pub(crate) fn parse_winget_search_candidate_line(line: &str) -> Option<SearchCandidate> {
    let mut parts = Vec::new();
    let mut in_token = false;
    let mut token_start = 0usize;

    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            if in_token {
                parts.push((token_start, &line[token_start..index]));
                in_token = false;
            }
        } else if !in_token {
            in_token = true;
            token_start = index;
        }
    }

    if in_token {
        parts.push((token_start, &line[token_start..]));
    }

    if parts.len() < 3 {
        return None;
    }

    let mut last_index = parts.len();
    let mut source = None;
    if !looks_like_version(parts[last_index - 1].1) {
        source = Some(parts[last_index - 1].1.to_string());
        last_index -= 1;
    }

    let version_index = (0..last_index)
        .rev()
        .find(|index| looks_like_version(parts[*index].1))?;
    if version_index == 0 {
        return None;
    }

    let (id_start, id) = parts[version_index - 1];
    if id.is_empty() {
        return None;
    }

    let name = line[..id_start].trim();
    let version = parts[version_index].1.trim();

    Some(SearchCandidate {
        backend: Backend::Winget,
        label: if name.is_empty() {
            id.to_string()
        } else {
            name.to_string()
        },
        install_id: id.to_string(),
        version: if version.is_empty() {
            None
        } else {
            Some(version.to_string())
        },
        source,
    })
}

pub(crate) fn parse_scoop_search_candidates(output: &str) -> Vec<SearchCandidate> {
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

pub(crate) fn parse_choco_search_candidates(output: &str) -> Vec<SearchCandidate> {
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

pub(crate) fn parse_npm_search_candidates(output: &str) -> Vec<SearchCandidate> {
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

pub(crate) fn parse_pip_search_candidates(output: &str) -> Vec<SearchCandidate> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // The prober emits a JSON array carrying the resolved version and summary.
    if trimmed.starts_with('[') {
        let Ok(JsonValue::Array(items)) = parse_json(trimmed) else {
            return Vec::new();
        };
        return items
            .iter()
            .filter_map(|item| {
                let name = item.get("name").and_then(JsonValue::as_str)?;
                let version = item.get("version").and_then(JsonValue::as_str);
                let summary = item.get("summary").and_then(JsonValue::as_str);
                let label = match summary.map(str::trim).filter(|text| !text.is_empty()) {
                    Some(summary) => format!("{name} - {summary}"),
                    None => name.to_string(),
                };
                Some(SearchCandidate {
                    backend: Backend::Pip,
                    label,
                    install_id: name.to_string(),
                    version: version.map(str::to_string),
                    source: Some("pypi".to_string()),
                })
            })
            .collect();
    }

    // Fallback: one bare project name per line.
    trimmed
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

pub(crate) fn parse_winget_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_winget_tabular_entries(output, false)
}

pub(crate) fn parse_winget_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_winget_tabular_entries(output, true)
}

pub(crate) fn parse_winget_tabular_entries(
    output: &str,
    upgradable: bool,
) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    let mut previous_line = "";
    let mut dashed_header_seen = false;
    let mut id_index = None;
    let mut version_index = None;
    let mut available_index = None;
    let mut source_index = None;

    for line in output.lines() {
        if !dashed_header_seen && line.contains("---") {
            let column_starts = infer_tabular_column_starts(line);
            id_index =
                find_char_index(previous_line, "Id").or_else(|| column_starts.get(1).copied());
            version_index =
                find_char_index(previous_line, "Version").or_else(|| column_starts.get(2).copied());
            available_index = find_char_index(previous_line, "Available").or_else(|| {
                if upgradable {
                    column_starts.get(3).copied()
                } else {
                    None
                }
            });
            source_index = find_char_index(previous_line, "Source").or_else(|| {
                if upgradable {
                    column_starts.get(4).copied()
                } else {
                    column_starts.get(3).copied()
                }
            });
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
                    let display_name = if name.is_empty() { id.clone() } else { name };
                    entries.push(PackageListEntry {
                        backend: Backend::Winget,
                        name: display_name,
                        package_id: id,
                        current_version: if current_version.is_empty() {
                            "-".to_string()
                        } else {
                            current_version
                        },
                        available_version: if upgradable { available_version } else { None },
                    });
                }
            } else if let Some(entry) = parse_winget_tabular_row_by_columns(line, upgradable) {
                entries.push(entry);
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

pub(crate) fn infer_tabular_column_starts(separator_line: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut in_dash_run = false;

    for (index, ch) in separator_line.chars().enumerate() {
        if ch == '-' {
            if !in_dash_run {
                starts.push(index);
                in_dash_run = true;
            }
        } else {
            in_dash_run = false;
        }
    }

    starts
}

pub(crate) fn parse_winget_tabular_row_by_columns(
    line: &str,
    upgradable: bool,
) -> Option<PackageListEntry> {
    let columns = split_columns_by_spacing(line);
    if upgradable {
        if columns.len() < 4 {
            return None;
        }
        let name = columns.first()?.to_string();
        let id = columns.get(1)?.to_string();
        let current_version = columns.get(2)?.to_string();
        let available_version = columns.get(3)?.to_string();
        if id.is_empty()
            || !looks_like_version(&current_version)
            || !looks_like_version(&available_version)
        {
            return None;
        }
        Some(PackageListEntry {
            backend: Backend::Winget,
            name,
            package_id: id,
            current_version: normalize_list_version(&current_version),
            available_version: Some(normalize_list_version(&available_version)),
        })
    } else {
        if columns.len() < 3 {
            return None;
        }
        let name = columns.first()?.to_string();
        let id = columns.get(1)?.to_string();
        let current_version = columns.get(2)?.to_string();
        if id.is_empty() || !looks_like_version(&current_version) {
            return None;
        }
        Some(PackageListEntry {
            backend: Backend::Winget,
            name,
            package_id: id,
            current_version: normalize_list_version(&current_version),
            available_version: None,
        })
    }
}

pub(crate) fn split_columns_by_spacing(line: &str) -> Vec<String> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut columns = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index].is_whitespace() {
            let whitespace_start = index;
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }

            if index - whitespace_start >= 2 {
                let value = chars[start..whitespace_start]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string();
                if !value.is_empty() {
                    columns.push(value);
                }
                start = index;
            }
        } else {
            index += 1;
        }
    }

    let tail = chars[start..].iter().collect::<String>().trim().to_string();
    if !tail.is_empty() {
        columns.push(tail);
    }

    columns
}

pub(crate) fn parse_scoop_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    let mut previous_line = "";
    let mut dashed_header_seen = false;
    let mut version_index = None;
    let mut source_index = None;

    for line in output.lines() {
        if !dashed_header_seen && line.contains("---") {
            version_index = find_char_index(previous_line, "Version");
            source_index = find_char_index(previous_line, "Source");
            dashed_header_seen = true;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("Installed apps")
            || trimmed.starts_with("Name")
            || trimmed.starts_with("---")
        {
            previous_line = line;
            continue;
        }

        if let (Some(version_index), Some(source_index)) = (version_index, source_index) {
            let name = slice_chars(line, 0, version_index).trim().to_string();
            let version = slice_chars(line, version_index, source_index)
                .trim()
                .to_string();

            if !name.is_empty() && !version.is_empty() {
                entries.push(PackageListEntry {
                    backend: Backend::Scoop,
                    name: name.clone(),
                    package_id: name,
                    current_version: normalize_list_version(&version),
                    available_version: None,
                });
            }
        } else {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            let Some(version_index) = parts.iter().position(|part| looks_like_version(part)) else {
                previous_line = line;
                continue;
            };
            if version_index == 0 {
                return None;
            }
            let version = parts[version_index];
            let name = parts[..version_index].join(" ");
            if name.is_empty() {
                return None;
            }

            entries.push(PackageListEntry {
                backend: Backend::Scoop,
                name: name.clone(),
                package_id: name,
                current_version: normalize_list_version(version),
                available_version: None,
            });
        }

        previous_line = line;
    }
    Some(entries)
}

pub(crate) fn parse_scoop_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
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
        let current = parts.last()?;
        if parts.len() < 2 {
            return None;
        }

        entries.push(PackageListEntry {
            backend: Backend::Scoop,
            name: parts[..parts.len() - 1]
                .join(" ")
                .trim_end_matches(':')
                .to_string(),
            package_id: parts[..parts.len() - 1]
                .join(" ")
                .trim_end_matches(':')
                .to_string(),
            current_version: normalize_list_version(current),
            available_version: Some(latest),
        });
    }
    Some(entries)
}

pub(crate) fn parse_choco_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let mut entries = Vec::new();
    let mut saw_package_line = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Chocolatey v") {
            continue;
        }
        // Chocolatey 2.7 emits retry/warning noise on stdout before the data
        // (e.g. "This is try 1/3..." when it cannot replace choco.exe.old).
        // Any line without the `name|version` shape is skipped rather than
        // aborting the whole parse, otherwise one warning hides every package.
        let Some((name, version)) = trimmed.split_once('|') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        saw_package_line = true;
        entries.push(PackageListEntry {
            backend: Backend::Chocolatey,
            name: name.to_string(),
            package_id: name.to_string(),
            current_version: normalize_list_version(version.trim()),
            available_version: None,
        });
    }

    // Distinguish "parsed fine, nothing installed" from "this is not list
    // output at all": only claim success when at least one row matched or the
    // text carried no pipe-separated content whatsoever.
    if !saw_package_line && output.contains('|') {
        return None;
    }
    Some(entries)
}

pub(crate) fn parse_choco_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
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
        // Same tolerance as the list parser: noise lines are skipped, not fatal.
        if parts.len() < 3 {
            continue;
        }

        entries.push(PackageListEntry {
            backend: Backend::Chocolatey,
            name: parts[0].trim().to_string(),
            package_id: parts[0].trim().to_string(),
            current_version: normalize_list_version(parts[1].trim()),
            available_version: Some(normalize_list_version(parts[2].trim())),
        });
    }
    Some(entries)
}

/// npm's `list --json` output. Parsed with the real JSON reader rather than a
/// line scanner: an entry may legitimately have no `version` key (npm emits
/// those for packages it considers "overridden" or partially installed), and
/// a line-based state machine silently swallowed every package after such an
/// entry -- which is how `cowsay` went missing from `waw list`.
pub(crate) fn parse_npm_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Some(Vec::new());
    }
    if !trimmed.starts_with('{') {
        return None;
    }

    let root = parse_json(trimmed).ok()?;
    let dependencies = root.get("dependencies")?.as_object()?;

    let mut entries = Vec::new();
    for (name, value) in dependencies {
        let version = value
            .get("version")
            .and_then(JsonValue::as_str)
            .unwrap_or("-");
        entries.push(PackageListEntry {
            backend: Backend::Npm,
            package_id: name.clone(),
            name: name.clone(),
            current_version: normalize_list_version(version),
            available_version: None,
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Some(entries)
}
pub(crate) fn parse_npm_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
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
                package_id: name.clone(),
                name,
                current_version: current_version.take().unwrap_or_else(|| "-".to_string()),
                available_version: latest_version.take(),
            });
        }
    }

    Some(entries)
}

pub(crate) fn parse_pip_list_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_pip_json_entries(output, false)
}

pub(crate) fn parse_pip_upgrade_entries(output: &str) -> Option<Vec<PackageListEntry>> {
    parse_pip_json_entries(output, true)
}

pub(crate) fn parse_pip_json_entries(
    output: &str,
    upgradable: bool,
) -> Option<Vec<PackageListEntry>> {
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
            package_id: name.clone(),
            name,
            current_version: version,
            available_version: latest,
        });
        idx = start + 6;
    }

    Some(entries)
}

pub(crate) fn parse_npm_show_details(output: &str) -> Option<PackageDetails> {
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

pub(crate) fn parse_npm_show_header(header: &str) -> Option<(String, String)> {
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

pub(crate) fn parse_npm_header_license(header: &str) -> Option<String> {
    header
        .split('|')
        .skip(1)
        .map(str::trim)
        .find(|segment| !segment.contains(':') && !segment.is_empty())
        .map(str::to_string)
}

pub(crate) fn parse_pip_show_details(output: &str) -> Option<PackageDetails> {
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
        .filter(|(label, value)| {
            !value.trim().is_empty()
                && !matches!(
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

pub(crate) const NAME_ALIASES: &[&str] = &[
    "Name",
    "Package Identifier",
    "PackageIdentifier",
    "Id",
    "Package",
    "名称",
];
pub(crate) const VERSION_ALIASES: &[&str] =
    &["Version", "Installed Version", "Latest Version", "版本"];
pub(crate) const SUMMARY_ALIASES: &[&str] = &[
    "Summary",
    "Description",
    "Short Description",
    "Moniker",
    "描述",
    "绰号",
];
pub(crate) const HOMEPAGE_ALIASES: &[&str] = &[
    "Homepage",
    "Home-page",
    "Project URL",
    "ProjectUrl",
    "Publisher URL",
    "PublisherUrl",
    "主页",
    "发布服务器 URL",
    "软件站点",
];
pub(crate) const LICENSE_ALIASES: &[&str] = &["License", "License Url", "LicenseUrl", "许可证"];
pub(crate) const AUTHOR_ALIASES: &[&str] = &["Author", "Publisher", "Maintainer", "作者", "发布者"];
pub(crate) const REPOSITORY_ALIASES: &[&str] = &[
    "Repository",
    "Repository Url",
    "RepositoryUrl",
    "存储库",
    "存储库 URL",
];
pub(crate) const KEYWORDS_ALIASES: &[&str] = &["Tags", "Keywords", "标记", "关键字"];
pub(crate) const DEPENDENCIES_ALIASES: &[&str] = &["Dependencies", "Requires", "依赖项", "依赖"];

pub(crate) fn parse_key_value_show_details(output: &str) -> Option<PackageDetails> {
    let fields = parse_colon_fields(output);
    let mut details = parse_details_from_fields(&fields);
    if details.name.trim().is_empty()
        && let Some((display, id)) = parse_winget_found_header(output)
    {
        details.name = if display.is_empty() { id } else { display };
    }
    if details.name.trim().is_empty() {
        return None;
    }
    Some(details)
}

pub(crate) fn parse_details_from_fields(fields: &[(String, String)]) -> PackageDetails {
    let name = find_field_value(fields, NAME_ALIASES).unwrap_or_default();
    let version = find_field_value(fields, VERSION_ALIASES).unwrap_or_else(|| "-".to_string());
    let summary = find_field_value(fields, SUMMARY_ALIASES);
    let homepage = find_field_value(fields, HOMEPAGE_ALIASES);
    let license = find_field_value(fields, LICENSE_ALIASES);
    let author = find_field_value(fields, AUTHOR_ALIASES);
    let repository = find_field_value(fields, REPOSITORY_ALIASES);
    let keywords = find_field_value(fields, KEYWORDS_ALIASES)
        .map(|value| split_list_value(&value))
        .unwrap_or_default();
    let dependencies = find_field_value(fields, DEPENDENCIES_ALIASES)
        .map(|value| split_list_value(&value))
        .unwrap_or_default();
    let extra_fields = fields
        .iter()
        .filter(|(label, value)| {
            !value.trim().is_empty()
                && !is_known_label(label, NAME_ALIASES)
                && !is_known_label(label, VERSION_ALIASES)
                && !is_known_label(label, SUMMARY_ALIASES)
                && !is_known_label(label, HOMEPAGE_ALIASES)
                && !is_known_label(label, LICENSE_ALIASES)
                && !is_known_label(label, AUTHOR_ALIASES)
                && !is_known_label(label, REPOSITORY_ALIASES)
                && !is_known_label(label, KEYWORDS_ALIASES)
                && !is_known_label(label, DEPENDENCIES_ALIASES)
        })
        .map(|(label, value)| (normalize_field_label(label), value.clone()))
        .collect();

    PackageDetails {
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
    }
}

pub(crate) fn is_known_label(label: &str, names: &[&str]) -> bool {
    names.iter().any(|name| label_matches(label, name))
}

pub(crate) fn parse_winget_found_header(output: &str) -> Option<(String, String)> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let open = line.find('[')?;
    let close = line[open..].find(']')? + open;
    let id = line[open + 1..close].trim().to_string();
    if id.is_empty() {
        return None;
    }
    let display = strip_found_prefix(line[..open].trim()).to_string();
    Some((display, id))
}

pub(crate) fn strip_found_prefix(value: &str) -> &str {
    for prefix in ["已找到", "Found", "found"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    value
}

pub(crate) fn parse_choco_show_details(output: &str) -> Option<PackageDetails> {
    let fields = parse_colon_fields(output);

    let title = find_field_value(&fields, &["Title", "标题"]);
    let package_line = parse_choco_package_line(output);
    let name = title
        .as_deref()
        .map(|value| value.split('|').next().unwrap_or(value).trim().to_string())
        .map(|value| match value.strip_suffix("(Install)") {
            Some(inner) if !inner.trim().is_empty() => inner.trim().to_string(),
            _ => value,
        })
        .or_else(|| package_line.as_ref().map(|(name, _)| name.clone()))?;

    let version = find_field_value(&fields, &["Version", "版本"])
        .or_else(|| package_line.as_ref().map(|(_, version)| version.clone()))
        .unwrap_or_else(|| "-".to_string());

    let summary = find_field_value(&fields, &["Summary", "摘要"])
        .or_else(|| find_field_value(&fields, &["Description", "描述"]));
    let homepage = find_field_value(
        &fields,
        &[
            "Software Site",
            "Project Site",
            "Package Url",
            "Package URL",
            "软件站点",
        ],
    );
    let license = find_field_value(&fields, &["Software License", "License", "许可证"]);
    let author = find_field_value(
        &fields,
        &["Author", "Publisher", "Maintainer", "作者", "发布者"],
    );
    let repository = find_field_value(
        &fields,
        &[
            "Software Source",
            "Chocolatey Package Source",
            "Project Source",
        ],
    );
    let keywords = find_field_value(&fields, &["Tags", "Keywords", "标记"])
        .map(|value| split_list_value(&value.replace(' ', ",")))
        .unwrap_or_default();

    let known = [
        "Title",
        "Summary",
        "Description",
        "Software Site",
        "Software License",
        "Software Source",
        "Package Url",
        "Package URL",
        "Tags",
        "Author",
        "Publisher",
        "Maintainer",
        "Chocolatey Package Source",
        "Project Source",
        "Version",
    ];
    let extra_fields = fields
        .iter()
        .filter(|(label, value)| {
            !value.trim().is_empty() && !known.iter().any(|name| label_matches(label, name))
        })
        .map(|(label, value)| (normalize_field_label(label), value.clone()))
        .collect();

    Some(PackageDetails {
        backend: Backend::Chocolatey,
        name,
        version,
        summary,
        homepage,
        license,
        author,
        repository,
        keywords,
        dependencies: Vec::new(),
        extra_fields,
    })
}

pub(crate) fn parse_choco_package_line(output: &str) -> Option<(String, String)> {
    let mut saw_banner = false;
    for line in output.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if line.contains(':') {
            continue;
        }
        if line.starts_with("Chocolatey v") || line.eq_ignore_ascii_case("chocolatey") {
            saw_banner = true;
            continue;
        }
        if !saw_banner {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let Some(version) = parts.next() else {
            continue;
        };
        if !version.starts_with(|ch: char| ch.is_ascii_digit()) {
            continue;
        }
        return Some((name.to_string(), version.to_string()));
    }
    None
}

pub(crate) fn parse_colon_fields(output: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();

    for raw_line in output.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() {
            continue;
        }

        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((index, width)) = split_field_line(line) else {
            if indented && let Some((_, value)) = fields.last_mut() {
                if !value.is_empty() {
                    value.push('\n');
                }
                value.push_str(line.trim());
            }
            continue;
        };

        let label = line[..index].trim();
        if label.is_empty() {
            continue;
        }
        let value = line[index + width..].trim().to_string();
        fields.push((label.to_string(), value));
    }

    fields
}

pub(crate) fn split_field_line(line: &str) -> Option<(usize, usize)> {
    let half = line.find(':').map(|index| (index, 1));
    let full = line
        .find('\u{FF1A}')
        .map(|index| (index, '\u{FF1A}'.len_utf8()));
    match (half, full) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(single), None) | (None, Some(single)) => Some(single),
        (None, None) => None,
    }
}

pub(crate) fn find_field_value(fields: &[(String, String)], names: &[&str]) -> Option<String> {
    fields
        .iter()
        .find(|(label, value)| {
            !value.trim().is_empty() && names.iter().any(|name| label_matches(label, name))
        })
        .map(|(_, value)| value.trim().to_string())
}

pub(crate) fn label_matches(label: &str, name: &str) -> bool {
    compact_label(label).eq_ignore_ascii_case(&compact_label(name))
}

pub(crate) fn compact_label(label: &str) -> String {
    label
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .map(|ch| if ch == '\u{FF1A}' { ':' } else { ch })
        .collect()
}

/// `winget pin list`: a localized header, a dashed rule, then one row each.
///
/// Columns are name / id / version / source / pin type. Both the name and the
/// id can contain spaces in principle, but winget pads with at least two
/// spaces between columns, so the split is on runs of whitespace.
pub(crate) fn parse_winget_pin_rows(output: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut seen_rule = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !seen_rule {
            // Localized headers mean structure is the only reliable signal.
            if trimmed.chars().all(|ch| ch == '-') && trimmed.len() > 3 {
                seen_rule = true;
            }
            continue;
        }

        let mut fields = trimmed.split_whitespace();
        let Some(_name) = fields.next() else { continue };
        let Some(id) = fields.next() else { continue };
        let pin_type = trimmed
            .split_whitespace()
            .last()
            .unwrap_or("pinned")
            .to_string();
        rows.push((id.to_string(), pin_type));
    }

    rows
}

/// `choco pin list`: a `Chocolatey vX.Y.Z` banner, then one name per line.
///
/// Names never contain spaces in Chocolatey, which is what makes a bare
/// line-per-name parse safe here.
pub(crate) fn parse_choco_pin_rows(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("Chocolatey v"))
        // Anything with a space is prose (a warning, or the empty-state note),
        // not a package name.
        .filter(|line| !line.contains(' '))
        .map(|line| (line.to_string(), "pinned".to_string()))
        .collect()
}

/// `scoop list` rows whose `Info` column contains `Held package`.
///
/// Scoop has no hold listing at all: `scoop hold` writes a flag into the app's
/// install manifest and `scoop list` renders it under `Info`. The columns are
/// found from the `Version`/`Source` headers, and `Info` is everything after
/// the source column.
pub(crate) fn parse_scoop_held_rows(output: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut previous_line = "";
    let mut version_index = None;
    let mut source_index = None;
    let mut saw_separator = false;

    for line in output.lines() {
        if !saw_separator && line.contains("---") {
            version_index = find_char_index(previous_line, "Version");
            source_index = find_char_index(previous_line, "Source");
            saw_separator = true;
            previous_line = line;
            continue;
        }

        if !saw_separator {
            previous_line = line;
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            previous_line = line;
            continue;
        }

        let (Some(version_index), Some(source_index)) = (version_index, source_index) else {
            previous_line = line;
            continue;
        };
        let source_index = source_index.max(version_index);

        // The name column ends where the version column starts.
        let name = slice_chars(line, 0, version_index).trim().to_string();
        // `Info` is the rest of the line after the source column, but the
        // source column itself has a fixed width, so look for the marker
        // anywhere past it.
        let tail = slice_chars(line, source_index, usize::MAX);
        if !name.is_empty() && tail.contains("Held package") {
            rows.push((name, "Held package".to_string()));
        }

        previous_line = line;
    }

    rows
}

pub(crate) fn split_list_value(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn normalize_field_label(label: &str) -> String {
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
