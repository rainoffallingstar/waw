//! Shared data types.
//!
//! These are the values every other module passes around: runtime settings,
//! search candidates, list rows, package details, captured command results,
//! and the planned invocation. Keeping them in one place means the parsers,
//! renderers, and executor all agree on the same shapes.

use crate::capture::unique_capture_base;
use crate::format_elapsed;
use crate::shell_escape;
use crate::Backend;
use crate::Config;
use std::io::{self, IsTerminal, Write};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RuntimeSettings<'a> {
    pub(crate) assume_yes: bool,
    pub(crate) auto_elevate: bool,
    pub(crate) command_timeout: Option<Duration>,
    /// `search --deep`: consult the full PyPI index instead of guessing names.
    pub(crate) deep_search: bool,
    pub(crate) config: &'a Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchCandidate {
    pub(crate) backend: Backend,
    pub(crate) label: String,
    pub(crate) install_id: String,
    pub(crate) version: Option<String>,
    pub(crate) source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoveTarget {
    pub(crate) package: String,
    pub(crate) version: Option<String>,
}

impl RemoveTarget {
    pub(crate) fn unversioned(package: String) -> Self {
        Self {
            package,
            version: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageListEntry {
    pub(crate) backend: Backend,
    pub(crate) name: String,
    pub(crate) package_id: String,
    pub(crate) current_version: String,
    pub(crate) available_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageDetails {
    pub(crate) backend: Backend,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) summary: Option<String>,
    pub(crate) homepage: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) repository: Option<String>,
    pub(crate) keywords: Vec<String>,
    pub(crate) dependencies: Vec<String>,
    pub(crate) extra_fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShowBackendResult {
    pub(crate) backend: Backend,
    pub(crate) command: String,
    pub(crate) success: bool,
    pub(crate) dry_run: bool,
    pub(crate) details: Option<PackageDetails>,
    pub(crate) raw_output: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCapture {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) success: bool,
    pub(crate) status_code: i32,
    pub(crate) timed_out: bool,
    /// The limit that applied to this run, for diagnostics. `None` means no
    /// limit was configured.
    pub(crate) timeout_seconds: Option<u64>,
}

pub(crate) struct CommandLogCapture {
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
}

impl CommandLogCapture {
    pub(crate) fn new() -> Result<Self, String> {
        let base = unique_capture_base("waw-command")?;
        Ok(Self {
            stdout_path: base.with_extension("stdout.log"),
            stderr_path: base.with_extension("stderr.log"),
        })
    }
}

impl Drop for CommandLogCapture {
    fn drop(&mut self) {
        // Removing here rather than at each success point keeps temporary files
        // from leaking when a command fails to spawn, a wait errors out, or any
        // other `?` returns early.
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

pub(crate) struct ProgressReporter {
    pub(crate) enabled: bool,
    pub(crate) frame_index: usize,
    pub(crate) last_width: usize,
    pub(crate) started_at: Instant,
}

impl ProgressReporter {
    pub(crate) fn new() -> Self {
        Self::new_with_enabled(true)
    }

    pub(crate) fn new_with_enabled(enabled: bool) -> Self {
        Self {
            enabled: enabled && io::stderr().is_terminal(),
            frame_index: 0,
            last_width: 0,
            started_at: Instant::now(),
        }
    }

    pub(crate) fn tick(&mut self, label: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
        let frame = FRAMES[self.frame_index % FRAMES.len()];
        self.frame_index += 1;
        self.render(&format!("{frame} {label}"))
    }

    pub(crate) fn finish(&mut self, success: bool, label: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        let status = if success { "ok" } else { "failed" };
        self.render(&format!(
            "{status} {label} ({})",
            format_elapsed(self.started_at.elapsed())
        ))?;
        io::stderr()
            .write_all(b"\n")
            .and_then(|_| io::stderr().flush())
            .map_err(|error| format!("failed to update progress output: {error}"))?;
        self.last_width = 0;
        Ok(())
    }

    pub(crate) fn render(&mut self, message: &str) -> Result<(), String> {
        let padding = self.last_width.saturating_sub(message.chars().count());
        let padded = if padding == 0 {
            message.to_string()
        } else {
            format!("{message}{}", " ".repeat(padding))
        };
        self.last_width = message.chars().count();
        io::stderr()
            .write_all(format!("\r{padded}").as_bytes())
            .and_then(|_| io::stderr().flush())
            .map_err(|error| format!("failed to update progress output: {error}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invocation {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) message: Option<String>,
    pub(crate) requires_elevation: bool,
}

impl Invocation {
    pub(crate) fn owned(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_string(),
            args,
            message: None,
            requires_elevation: false,
        }
    }

    pub(crate) fn message(message: &str) -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            message: Some(message.to_string()),
            requires_elevation: false,
        }
    }

    pub(crate) fn with_elevation(mut self, requires_elevation: bool) -> Self {
        self.requires_elevation = requires_elevation;
        self
    }

    pub(crate) fn render_for_display(&self) -> String {
        if self.program.is_empty() {
            return String::new();
        }

        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(shell_escape(&self.program));
        parts.extend(self.args.iter().map(|arg| shell_escape(arg)));
        parts.join(" ")
    }
}
