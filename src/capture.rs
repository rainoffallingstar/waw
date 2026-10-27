//! Temp-file naming and captured-output decoding.
//!
//! Backend tools write UTF-8 or UTF-16 depending on the tool and the
//! console code page, so captured bytes are decoded by sniffing the
//! encoding rather than assuming one.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

static CAPTURE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn unique_capture_base(prefix: &str) -> Result<PathBuf, String> {
    // The wall clock alone is not enough: `SystemTime` granularity on Windows is
    // about 15 ms, so parallel backend threads would produce identical names and
    // clobber each other's capture files. The atomic counter makes every call
    // unique within the process.
    let sequence = CAPTURE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("failed to build capture timestamp: {error}"))?
        .as_nanos();
    Ok(env::temp_dir().join(format!(
        "{prefix}-{}-{nanos}-{sequence}",
        std::process::id()
    )))
}

pub(crate) fn read_capture_log(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| decode_capture_text(&bytes, true))
        .unwrap_or_default()
}

pub(crate) fn decode_capture_text(bytes: &[u8], final_chunk: bool) -> String {
    let (encoding, skip) = detect_capture_encoding(bytes);
    let body = &bytes[skip..];
    match encoding {
        CaptureEncoding::Utf8 => decode_utf8_capture(body, final_chunk),
        CaptureEncoding::Utf16Le => decode_utf16_capture(body, final_chunk, true),
        CaptureEncoding::Utf16Be => decode_utf16_capture(body, final_chunk, false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

pub(crate) fn detect_capture_encoding(bytes: &[u8]) -> (CaptureEncoding, usize) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (CaptureEncoding::Utf8, 3)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (CaptureEncoding::Utf16Le, 2)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (CaptureEncoding::Utf16Be, 2)
    } else if looks_like_utf16_le(bytes) {
        (CaptureEncoding::Utf16Le, 0)
    } else if looks_like_utf16_be(bytes) {
        (CaptureEncoding::Utf16Be, 0)
    } else {
        (CaptureEncoding::Utf8, 0)
    }
}

pub(crate) fn looks_like_utf16_le(bytes: &[u8]) -> bool {
    looks_like_utf16(bytes, true)
}

pub(crate) fn looks_like_utf16_be(bytes: &[u8]) -> bool {
    looks_like_utf16(bytes, false)
}

pub(crate) fn looks_like_utf16(bytes: &[u8], little_endian: bool) -> bool {
    let sample = bytes.len().min(64);
    if sample < 2 {
        return false;
    }

    let mut zero_slots = 0usize;
    let mut zero_matches = 0usize;
    let mut nonzero_slots = 0usize;
    let mut nonzero_matches = 0usize;

    for (index, byte) in bytes[..sample].iter().enumerate() {
        let zero_expected = if little_endian {
            index % 2 == 1
        } else {
            index % 2 == 0
        };

        if zero_expected {
            zero_slots += 1;
            if *byte == 0 {
                zero_matches += 1;
            }
        } else {
            nonzero_slots += 1;
            if *byte != 0 {
                nonzero_matches += 1;
            }
        }
    }

    zero_slots >= 2
        && zero_matches * 4 >= zero_slots * 3
        && nonzero_slots >= 2
        && nonzero_matches * 4 >= nonzero_slots * 3
}

pub(crate) fn decode_utf8_capture(bytes: &[u8], final_chunk: bool) -> String {
    if final_chunk {
        return String::from_utf8_lossy(bytes).into_owned();
    }

    let mut output = String::new();
    let mut start = 0usize;
    while start < bytes.len() {
        match std::str::from_utf8(&bytes[start..]) {
            Ok(valid) => {
                output.push_str(valid);
                return output;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    output.push_str(
                        std::str::from_utf8(&bytes[start..start + valid_up_to])
                            .expect("valid utf-8 prefix"),
                    );
                    start += valid_up_to;
                }

                match error.error_len() {
                    Some(error_len) => {
                        output.push('\u{FFFD}');
                        start += error_len;
                    }
                    None => return output,
                }
            }
        }
    }

    output
}

pub(crate) fn decode_utf16_capture(bytes: &[u8], final_chunk: bool, little_endian: bool) -> String {
    let even_len = bytes.len() - (bytes.len() % 2);
    let mut units = bytes[..even_len]
        .chunks_exact(2)
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    let had_odd_tail = bytes.len() % 2 == 1;

    if !final_chunk {
        if matches!(units.last(), Some(unit) if (0xD800..=0xDBFF).contains(unit)) {
            units.pop();
        }
        return String::from_utf16_lossy(&units);
    }

    let mut output = String::from_utf16_lossy(&units);
    if had_odd_tail {
        output.push('\u{FFFD}');
    }
    output
}
