//! Minimal JSON reader and writer helpers.
//!
//! `waw` has no dependencies, so manifests and JSON payloads are handled by
//! this small recursive-descent reader.

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }
}

/// Minimal recursive-descent JSON reader. It exists so `import` can read a
/// manifest without pulling in a dependency; it accepts standard JSON and
/// rejects anything it cannot fully understand.
pub fn parse_json(input: &str) -> Result<JsonValue, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut position = 0usize;
    let value = parse_json_value(&chars, &mut position)?;
    skip_json_whitespace(&chars, &mut position);
    if position != chars.len() {
        return Err(format!(
            "unexpected trailing JSON content at character {position}"
        ));
    }
    Ok(value)
}

fn skip_json_whitespace(chars: &[char], position: &mut usize) {
    while *position < chars.len() && chars[*position].is_whitespace() {
        *position += 1;
    }
}

fn parse_json_value(chars: &[char], position: &mut usize) -> Result<JsonValue, String> {
    skip_json_whitespace(chars, position);
    let Some(current) = chars.get(*position).copied() else {
        return Err("unexpected end of JSON input".to_string());
    };
    match current {
        '{' => parse_json_object(chars, position),
        '[' => parse_json_array(chars, position),
        '"' => parse_json_string(chars, position).map(JsonValue::String),
        't' | 'f' | 'n' => parse_json_literal(chars, position),
        _ => parse_json_number(chars, position),
    }
}

fn parse_json_object(chars: &[char], position: &mut usize) -> Result<JsonValue, String> {
    *position += 1; // consume '{'
    let mut entries = Vec::new();
    skip_json_whitespace(chars, position);
    if chars.get(*position) == Some(&'}') {
        *position += 1;
        return Ok(JsonValue::Object(entries));
    }
    loop {
        skip_json_whitespace(chars, position);
        let key = parse_json_string(chars, position)?;
        skip_json_whitespace(chars, position);
        if chars.get(*position) != Some(&':') {
            return Err(format!("expected ':' after object key at {position}"));
        }
        *position += 1;
        let value = parse_json_value(chars, position)?;
        entries.push((key, value));
        skip_json_whitespace(chars, position);
        match chars.get(*position) {
            Some(',') => {
                *position += 1;
            }
            Some('}') => {
                *position += 1;
                return Ok(JsonValue::Object(entries));
            }
            _ => return Err(format!("expected ',' or '}}' in object at {position}")),
        }
    }
}

fn parse_json_array(chars: &[char], position: &mut usize) -> Result<JsonValue, String> {
    *position += 1; // consume '['
    let mut values = Vec::new();
    skip_json_whitespace(chars, position);
    if chars.get(*position) == Some(&']') {
        *position += 1;
        return Ok(JsonValue::Array(values));
    }
    loop {
        values.push(parse_json_value(chars, position)?);
        skip_json_whitespace(chars, position);
        match chars.get(*position) {
            Some(',') => {
                *position += 1;
            }
            Some(']') => {
                *position += 1;
                return Ok(JsonValue::Array(values));
            }
            _ => return Err(format!("expected ',' or ']' in array at {position}")),
        }
    }
}

fn parse_json_string(chars: &[char], position: &mut usize) -> Result<String, String> {
    if chars.get(*position) != Some(&'"') {
        return Err(format!("expected string at {position}"));
    }
    *position += 1;
    let mut value = String::new();
    loop {
        let Some(current) = chars.get(*position).copied() else {
            return Err("unterminated JSON string".to_string());
        };
        *position += 1;
        match current {
            '"' => return Ok(value),
            '\\' => {
                let Some(escape) = chars.get(*position).copied() else {
                    return Err("unterminated JSON escape".to_string());
                };
                *position += 1;
                match escape {
                    '"' => value.push('"'),
                    '\\' => value.push('\\'),
                    '/' => value.push('/'),
                    'b' => value.push('\u{8}'),
                    'f' => value.push('\u{c}'),
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let Some(digit) = chars.get(*position).and_then(|ch| ch.to_digit(16))
                            else {
                                return Err("invalid \\u escape in JSON string".to_string());
                            };
                            code = code * 16 + digit;
                            *position += 1;
                        }
                        value.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    other => return Err(format!("unsupported JSON escape \\{other}")),
                }
            }
            other => value.push(other),
        }
    }
}

fn parse_json_literal(chars: &[char], position: &mut usize) -> Result<JsonValue, String> {
    for (literal, value) in [
        ("true", JsonValue::Bool(true)),
        ("false", JsonValue::Bool(false)),
        ("null", JsonValue::Null),
    ] {
        let end = *position + literal.chars().count();
        if end <= chars.len() && chars[*position..end].iter().copied().eq(literal.chars()) {
            *position = end;
            return Ok(value);
        }
    }
    Err(format!("invalid JSON literal at {position}"))
}

fn parse_json_number(chars: &[char], position: &mut usize) -> Result<JsonValue, String> {
    let start = *position;
    if chars.get(*position) == Some(&'-') {
        *position += 1;
    }
    while let Some(ch) = chars.get(*position) {
        if ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-') {
            *position += 1;
        } else {
            break;
        }
    }
    if start == *position {
        return Err(format!("invalid JSON value at {position}"));
    }
    let raw: String = chars[start..*position].iter().collect();
    let parsed: f64 = raw
        .parse()
        .map_err(|_| format!("invalid JSON number at {start}: {raw}"))?;
    Ok(JsonValue::Number(parsed))
}

pub fn escape_json_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
