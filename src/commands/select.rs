//! Interactive package selection shared by install, remove and upgrade.
//!
//! These prompt on a real terminal and refuse to guess on a pipe, so a CI
//! runner never blocks on stdin waiting for a selection nobody can make.

use super::super::*;

pub(crate) fn print_candidates(candidates: &[SearchCandidate]) {
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

pub(crate) fn choose_candidates(
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
                candidate
                    .label
                    .eq_ignore_ascii_case(candidate.install_id.as_str())
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

pub(crate) fn interactive_stdin_available() -> bool {
    io::stdin().is_terminal()
        || env::var("WAW_FORCE_INTERACTIVE")
            .map(|value| !value.trim().is_empty() && value.trim() != "0")
            .unwrap_or(false)
}

pub(crate) fn prompt_for_selection(action: &str, max: usize) -> Result<Vec<usize>, String> {
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

pub(crate) fn parse_selection(input: &str, max: usize) -> Result<Vec<usize>, String> {
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

pub(crate) fn parse_index(raw: &str, max: usize) -> Result<usize, String> {
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
