//! Command execution: running planned invocations.
//!
//! Handles the two execution modes -- a single elevated batch versus direct
//! spawning -- plus capture plumbing, timeouts, and process-tree teardown.
//! Parallel capture is used for read-only collection across backends; the
//! mutating path stays serial.

use crate::capture::read_capture_log;
use crate::elevation::*;
use crate::model::*;
use crate::Backend;
use std::fs;
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::win32::{CloseHandle, CREATE_SUSPENDED, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ProcessJob, resume_process_threads};

pub(crate) fn execute_invocations(
    invocations: Vec<Invocation>,
    dry_run: bool,
    continue_on_error: bool,
    runtime: &RuntimeSettings<'_>,
) -> Result<ExitCode, String> {
    let process_is_elevated = runtime.auto_elevate && is_process_elevated();
    let should_batch_elevate = cfg!(windows)
        && runtime.auto_elevate
        && !process_is_elevated
        && invocations
            .iter()
            .any(|invocation| invocation.requires_elevation && !invocation.program.is_empty());

    for invocation in &invocations {
        if let Some(message) = invocation.message.as_deref() {
            println!("{message}");
        }

        if invocation.program.is_empty() {
            continue;
        }

        if dry_run {
            println!("> {}", invocation.render_for_display());
            continue;
        }
    }

    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    if should_batch_elevate {
        run_elevated_invocations(&invocations, continue_on_error, runtime.command_timeout)?;
        return Ok(ExitCode::SUCCESS);
    }

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();

    for invocation in invocations {
        if invocation.program.is_empty() {
            continue;
        }

        let display_command = invocation.render_for_display();
        let progress_label = invocation_progress_label(&invocation);
        let capture = if cfg!(windows)
            && runtime.auto_elevate
            && invocation.requires_elevation
            && !process_is_elevated
        {
            println!("requesting administrator privileges...");
            run_elevated_invocation(&invocation, &progress_label, runtime.command_timeout)?
        } else {
            run_logged_command_capture_with_label(
                &invocation,
                &progress_label,
                true,
                runtime.command_timeout,
                |_| false,
            )?
        };

        if !capture.success {
            if is_tolerable_command_failure(&invocation, &capture) {
                if continue_on_error {
                    succeeded.push(progress_label);
                }
                continue;
            }
            emit_command_logs(&display_command, &capture.stdout, &capture.stderr);
            let error = render_command_failure(&invocation, &capture);
            if continue_on_error {
                failed.push((progress_label, error));
                continue;
            }
            return Err(error);
        }

        if continue_on_error {
            succeeded.push(progress_label);
        }
    }

    if continue_on_error && !failed.is_empty() {
        if let Some(summary) = render_invocation_batch_summary(&succeeded, &failed) {
            eprintln!("{summary}");
        }
        return Err(format!(
            "{} command(s) failed during execution",
            failed.len()
        ));
    }

    Ok(ExitCode::SUCCESS)
}

pub(crate) fn run_capture_detailed_with_label(
    invocation: &Invocation,
    label: &str,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    run_capture_detailed_with_label_when(invocation, label, false, timeout, |_| false)
}

pub(crate) fn run_capture_detailed_with_label_when<F>(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: F,
) -> Result<CommandCapture, String>
where
    F: FnOnce(&CommandCapture) -> bool,
{
    run_logged_command_capture_with_label(invocation, label, inherit_stdin, timeout, accept_capture)
}

pub(crate) fn render_command_failure(invocation: &Invocation, capture: &CommandCapture) -> String {
    if capture.timed_out {
        return format!(
            "backend command timed out after {}s: {}",
            capture.timeout_seconds.unwrap_or(0),
            invocation.render_for_display()
        );
    }
    let stderr = capture.stderr.trim();
    if stderr.is_empty() {
        format!(
            "backend command failed with exit code {}: {}",
            capture.status_code,
            invocation.render_for_display()
        )
    } else {
        format!(
            "backend command failed with exit code {}: {} ({stderr})",
            capture.status_code,
            invocation.render_for_display()
        )
    }
}

pub(crate) fn render_backend_output_section(backend: Backend, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("== {backend} ==\n{trimmed}"))
    }
}

pub(crate) fn run_logged_command_capture_with_label(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: impl FnOnce(&CommandCapture) -> bool,
) -> Result<CommandCapture, String> {
    run_logged_command_capture_inner(
        invocation,
        label,
        inherit_stdin,
        timeout,
        accept_capture,
        false,
    )
}

pub(crate) fn run_quiet_command_capture(
    invocation: &Invocation,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    run_logged_command_capture_inner(invocation, "", false, timeout, |_| false, true)
}

/// Terminates a timed-out child and everything it spawned.
///
/// On Windows the child is attached to a job object at spawn time, so the whole
/// tree can be torn down in one call. `Child::kill` alone only reaches the
/// wrapper (`scoop.cmd`, `npm.cmd`, ...) and leaves grandchildren running.
pub(crate) fn kill_process_tree(child: &mut std::process::Child, job: Option<&ProcessJobHandle>) {
    #[cfg(windows)]
    if let Some(job) = job {
        job.terminate(1);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
type ProcessJobHandle = ProcessJob;
#[cfg(not(windows))]
type ProcessJobHandle = ();

pub(crate) fn run_logged_command_capture_inner(
    invocation: &Invocation,
    label: &str,
    inherit_stdin: bool,
    timeout: Option<Duration>,
    accept_capture: impl FnOnce(&CommandCapture) -> bool,
    quiet: bool,
) -> Result<CommandCapture, String> {
    let capture = CommandLogCapture::new()?;
    let stdout_file = fs::File::create(&capture.stdout_path)
        .map_err(|error| format!("failed to open stdout capture file: {error}"))?;
    let stderr_file = fs::File::create(&capture.stderr_path)
        .map_err(|error| format!("failed to open stderr capture file: {error}"))?;
    let progress_label = compact_progress_label(label);

    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(if inherit_stdin {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    // On Windows the child is started suspended so that it can be attached to
    // a job object before any of its threads run. That way a timed-out backend
    // takes down the wrapper script's grandchildren too, not just the wrapper.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {}: {error}", invocation.program))?;

    #[cfg(windows)]
    let job = {
        let job = ProcessJob::create();
        let pid = child.id();
        if let Some(active_job) = job.as_ref() {
            let process_handle = unsafe {
                OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid)
            };
            if !process_handle.is_null() {
                let assigned = active_job.assign(process_handle);
                unsafe {
                    CloseHandle(process_handle);
                }
                if !assigned {
                    // Assignment can fail when the process already belongs to a
                    // job with a conflicting limit; fall back to a plain kill.
                }
            }
        }
        resume_process_threads(pid);
        job
    };

    #[cfg(not(windows))]
    let job: Option<()> = None;

    let mut reporter = ProgressReporter::new_with_enabled(!quiet);
    let started_at = Instant::now();
    let mut timed_out = false;

    loop {
        reporter.tick(&progress_label)?;
        if child
            .try_wait()
            .map_err(|error| format!("failed to monitor {}: {error}", invocation.program))?
            .is_some()
        {
            break;
        }
        if timeout.is_some_and(|limit| started_at.elapsed() >= limit) {
            kill_process_tree(&mut child, job.as_ref());
            timed_out = true;
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }

    let status = if timed_out {
        None
    } else {
        Some(
            child
                .wait()
                .map_err(|error| format!("failed to collect {} result: {error}", invocation.program))?,
        )
    };

    let stdout = read_capture_log(&capture.stdout_path);
    let stderr = read_capture_log(&capture.stderr_path);

    let command_capture = CommandCapture {
        stdout,
        stderr,
        success: status.as_ref().is_some_and(|status| status.success()),
        status_code: status.as_ref().and_then(|status| status.code()).unwrap_or(1),
        timed_out,
        timeout_seconds: timeout.map(|limit| limit.as_secs()),
    };
    reporter.finish(
        command_capture.success || accept_capture(&command_capture),
        &progress_label,
    )?;

    Ok(command_capture)
}

pub(crate) fn render_invocation_batch_summary(
    succeeded: &[String],
    failed: &[(String, String)],
) -> Option<String> {
    if succeeded.is_empty() && failed.is_empty() {
        return None;
    }

    let mut lines = vec!["Batch summary:".to_string()];
    if !succeeded.is_empty() {
        lines.push(format!("Succeeded ({}):", succeeded.len()));
        lines.extend(succeeded.iter().map(|label| format!("- {label}")));
    }
    if !failed.is_empty() {
        lines.push(format!("Failed ({}):", failed.len()));
        lines.extend(
            failed
                .iter()
                .map(|(label, reason)| format!("- {label}: {reason}")),
        );
    }

    Some(lines.join("\n"))
}

pub(crate) fn emit_command_logs(command: &str, stdout: &str, stderr: &str) {
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();

    if stdout.is_empty() && stderr.is_empty() {
        return;
    }

    eprintln!("--- logs for `{command}` ---");
    if !stdout.is_empty() {
        eprintln!("[stdout]");
        eprintln!("{stdout}");
    }
    if !stderr.is_empty() {
        eprintln!("[stderr]");
        eprintln!("{stderr}");
    }
}

pub(crate) fn emit_successful_command_output(stdout: &str, stderr: &str) {
    let stdout = stdout.trim_end();
    let stderr = stderr.trim_end();

    if !stdout.is_empty() {
        println!("{stdout}");
    }
    if !stderr.is_empty() {
        eprintln!("{stderr}");
    }
}

pub(crate) fn compact_progress_label(label: &str) -> String {
    const MAX_CHARS: usize = 64;
    let mut compact = label.replace('\n', " ");
    compact = compact.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = compact.chars().count();
    if count <= MAX_CHARS {
        compact
    } else {
        let shortened = compact.chars().take(MAX_CHARS - 1).collect::<String>();
        format!("{shortened}…")
    }
}

pub(crate) fn invocation_progress_label(invocation: &Invocation) -> String {
    let backend = invocation_backend_name(invocation);
    let args = invocation.args.as_slice();
    let target = invocation_target_label(invocation);

    if matches!(args, [action] if action == "update") && backend == "scoop" {
        return "Updating scoop buckets".to_string();
    }
    if matches!(args, [action, target] if action == "update" && target == "*") {
        return format!("Upgrading {backend} packages");
    }
    if matches!(args, [action, target] if action == "update" && target == "--global") {
        return "Upgrading npm global packages".to_string();
    }
    if args.first().map(String::as_str) == Some("update") {
        return match target {
            Some(target) => format!("Upgrading {backend}: {target}"),
            None => format!("Updating {backend} package"),
        };
    }
    if args.len() >= 2 && args[0] == "source" && args[1] == "update" {
        return format!("Updating {backend} sources");
    }
    if args.len() >= 2 && args[0] == "upgrade" && args[1] == "--all" {
        return format!("Upgrading {backend} packages");
    }
    if args.first().map(String::as_str) == Some("upgrade") {
        return match target {
            Some(target) => format!("Upgrading {backend}: {target}"),
            None => format!("Upgrading {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("install") {
        return match target {
            Some(target) => format!("Installing {backend}: {target}"),
            None => format!("Installing {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("uninstall") {
        return match target {
            Some(target) => format!("Removing {backend}: {target}"),
            None => format!("Removing {backend} package"),
        };
    }
    if args.first().map(String::as_str) == Some("search") && args.len() >= 2 {
        return format!("Searching {backend} for {}", args[1]);
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "list" {
        if args.iter().any(|arg| arg == "--outdated") {
            return "Checking pip updates".to_string();
        }
        return "Listing pip packages".to_string();
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "install" {
        return match target {
            Some(target) if args.iter().any(|arg| arg == "--upgrade") => {
                format!("Upgrading pip: {target}")
            }
            Some(target) => format!("Installing pip: {target}"),
            None if args.iter().any(|arg| arg == "--upgrade") => {
                "Upgrading pip packages".to_string()
            }
            None => "Installing pip packages".to_string(),
        };
    }
    if args.len() >= 3 && args[0] == "-m" && args[1] == "pip" && args[2] == "show" {
        return "Showing pip package details".to_string();
    }
    if args.len() >= 3
        && args[0] == "-c"
        && let Some(query) = args.last()
    {
        return format!("Searching pip for {query}");
    }
    if args.first().map(String::as_str) == Some("status") {
        return format!("Checking {backend} updates");
    }
    if args.first().map(String::as_str) == Some("list") {
        return format!("Listing {backend} packages");
    }
    if args.first().map(String::as_str) == Some("show")
        || args.first().map(String::as_str) == Some("info")
        || args.first().map(String::as_str) == Some("view")
    {
        return format!("Showing {backend} package details");
    }

    format!("Running {backend}")
}

pub(crate) fn invocation_target_label(invocation: &Invocation) -> Option<String> {
    let backend = invocation_backend_name(invocation);
    let args = invocation.args.as_slice();

    match backend {
        "winget" => argument_value_after(args, "--id")
            .or_else(|| argument_value_after(args, "--name"))
            .or_else(|| positional_argument(args, 1)),
        "scoop" | "choco" | "npm" => positional_argument(args, 1),
        "pip" => pip_invocation_target(args),
        _ => positional_argument(args, 1),
    }
}

pub(crate) fn argument_value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

pub(crate) fn positional_argument(args: &[String], start: usize) -> Option<String> {
    args.iter()
        .skip(start)
        .find(|arg| !arg.starts_with('-'))
        .cloned()
}

pub(crate) fn pip_invocation_target(args: &[String]) -> Option<String> {
    if args.len() < 4 || args[0] != "-m" || args[1] != "pip" {
        return None;
    }

    positional_argument(args, 3)
}

pub(crate) fn invocation_backend_name(invocation: &Invocation) -> &'static str {
    let program = invocation.program.replace('\\', "/").to_ascii_lowercase();
    if invocation.args.len() >= 2 && invocation.args[0] == "-m" && invocation.args[1] == "pip" {
        return "pip";
    }
    if program.contains("winget") {
        "winget"
    } else if program.contains("scoop") {
        "scoop"
    } else if program.contains("choco") {
        "choco"
    } else if program.contains("npm") {
        "npm"
    } else if program.contains("python") || program.ends_with("/py.exe") || program.ends_with("/py")
    {
        "pip"
    } else {
        "command"
    }
}
