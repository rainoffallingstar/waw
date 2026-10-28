//! Small string helpers shared by the parsers and the search ranking.
//!
//! These exist because the backends emit localized, column-aligned text with
//! no machine-readable alternative, so a lot of this crate is character
//! slicing rather than structured parsing.

pub(crate) fn extract_json_string_value_after_key(input: &str, start: usize, key: &str) -> Option<String> {
    input[start..]
        .find(key)
        .and_then(|offset| extract_json_string_value(input, start + offset))
}

pub(crate) fn extract_json_string_value(input: &str, key_pos: usize) -> Option<String> {
    let after_colon = input[key_pos..].find(':')? + key_pos + 1;
    let first_quote = input[after_colon..].find('"')? + after_colon + 1;
    let mut escaped = false;
    let mut value = String::new();
    for ch in input[first_quote..].chars() {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return Some(value);
        }
        value.push(ch);
    }
    None
}

pub(crate) fn starts_with_version_char(value: &str) -> bool {
    value
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
}

pub(crate) fn looks_like_version(value: &str) -> bool {
    starts_with_version_char(value.trim_matches(|ch| ch == '(' || ch == ')' || ch == ':'))
}

pub(crate) fn normalize_list_version(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch| ch == '(' || ch == ')' || ch == ':')
        .to_string()
}

pub(crate) fn looks_like_url(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("http://") || value.starts_with("https://")
}

pub(crate) fn find_char_index(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .find(needle)
        .map(|byte_index| haystack[..byte_index].chars().count())
}

pub(crate) fn slice_chars(value: &str, start: usize, end: usize) -> String {
    value
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}
