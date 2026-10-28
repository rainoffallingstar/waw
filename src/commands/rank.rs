//! Ordering search results across backends.
//!
//! The goal is that typing a name and hitting enter picks the thing a human
//! would: an exact id or name beats a prefix, which beats a substring, and a
//! backend listed first in config wins ties.

use super::super::*;

pub(crate) fn dedupe_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
    dedupe_search_candidates(candidates)
}

pub(crate) fn dedupe_search_candidates(candidates: Vec<SearchCandidate>) -> Vec<SearchCandidate> {
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

pub(crate) fn dedupe_installed_candidates(
    candidates: Vec<SearchCandidate>,
) -> Vec<SearchCandidate> {
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing: &SearchCandidate| {
            existing.backend == candidate.backend
                && existing
                    .install_id
                    .eq_ignore_ascii_case(&candidate.install_id)
                && existing.label.eq_ignore_ascii_case(&candidate.label)
                && existing.version == candidate.version
        }) {
            deduped.push(candidate);
        }
    }
    deduped
}

pub(crate) fn sort_search_candidates(
    mut candidates: Vec<SearchCandidate>,
    query: &str,
    backends: &[Backend],
) -> Vec<SearchCandidate> {
    let query = normalize_search_text(query);

    // pip already ranks its own results: the prober compares publication dates
    // so a maintained `beautifulsoup4` outranks the abandoned `BeautifulSoup`.
    // Re-sorting here with the generic prefix heuristic would undo that, since
    // "BeautifulSoup" is an exact match and would win on rank alone.
    if candidates
        .iter()
        .all(|candidate| candidate.backend == Backend::Pip)
    {
        return candidates;
    }

    candidates.sort_by(|left, right| {
        let left_key = search_sort_key(left, &query, backends);
        let right_key = search_sort_key(right, &query, backends);
        left_key.cmp(&right_key)
    });
    candidates
}

pub(crate) fn search_sort_key(
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

pub(crate) fn search_match_rank(label: &str, install_id: &str, query: &str) -> u8 {
    if query.is_empty() {
        return 3;
    }

    let compact_query = compact_search_text(query);
    let compact_label = compact_search_text(label);
    let compact_install_id = compact_search_text(install_id);

    if label == query
        || install_id == query
        || (!compact_query.is_empty()
            && (compact_label == compact_query || compact_install_id == compact_query))
    {
        0
    } else if label.starts_with(query)
        || install_id.starts_with(query)
        || (!compact_query.is_empty()
            && (compact_label.starts_with(&compact_query)
                || compact_install_id.starts_with(&compact_query)))
    {
        1
    } else if label.contains(query)
        || install_id.contains(query)
        || (!compact_query.is_empty()
            && (compact_label.contains(&compact_query)
                || compact_install_id.contains(&compact_query)))
    {
        2
    } else {
        3
    }
}

pub(crate) fn backend_rank_for_search(backend: Backend, backends: &[Backend]) -> usize {
    backends
        .iter()
        .position(|candidate| *candidate == backend)
        .unwrap_or(backends.len())
}

pub(crate) fn normalize_search_text(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(crate) fn compact_search_text(value: &str) -> String {
    normalize_search_text(value)
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}
