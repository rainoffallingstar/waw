//! Windows elevation: launching commands through UAC.
//!
//! Mutating operations may need administrator rights. Rather than one UAC
//! prompt per package, a batch of commands is written to a script, launched
//! once through ShellExecuteEx with the `runas` verb, and progress is
//! reported back through marker lines in a status log.

use super::*;

pub(crate) fn is_process_elevated() -> bool {
    if !cfg!(windows) {
        return false;
    }

    Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[bool](([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

pub(crate) fn run_current_process_elevated() -> Result<CommandCapture, String> {
    let current_exe = env::current_exe()
        .map_err(|error| format!("failed to resolve current executable for elevation: {error}"))?;
    let args: Vec<OsString> = env::args_os().skip(1).collect();

    run_elevated_program(
        current_exe.as_os_str(),
        &args,
        "Running elevated command",
        None,
    )
}

pub(crate) fn run_elevated_invocation(
    invocation: &Invocation,
    label: &str,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    let args: Vec<OsString> = invocation.args.iter().map(OsString::from).collect();

    run_elevated_program(OsStr::new(&invocation.program), &args, label, timeout)
}

pub(crate) fn run_elevated_invocations(
    invocations: &[Invocation],
    continue_on_error: bool,
    timeout: Option<Duration>,
) -> Result<(), String> {
    let capture = ElevationCapture::new()?;
    let working_directory = env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let inner_command = build_elevated_batch_command(
        invocations,
        capture.stdout_path.as_os_str(),
        capture.stderr_path.as_os_str(),
        capture.status_path.as_os_str(),
        continue_on_error,
    );
    write_elevated_command_file(&capture.command_path, &inner_command)?;
    let outcome = run_logged_elevated_command_file(
        capture.command_path.as_os_str(),
        &capture,
        "elevated command batch",
        &working_directory,
        timeout,
    )?;
    if outcome.status.success() {
        // The elevated child writes its progress to a status log that the
        // caller never sees, so report completion here instead of exiting
        // silently after a successful batch.
        for invocation in invocations {
            if invocation.program.is_empty() {
                continue;
            }
            println!("ok {}", crate::invocation_progress_label(invocation));
        }
        Ok(())
    } else {
        if let Some(summary) = render_elevated_batch_summary(invocations, &outcome.status_log) {
            eprintln!("{summary}");
        }
        emit_command_logs(
            "elevated command batch",
            &outcome.stdout_log,
            &visible_elevated_stderr(&outcome.stderr_log),
        );
        Err(format_elevated_batch_failure(
            outcome.status.code().unwrap_or(1),
            &outcome.status_log,
        ))
    }
}

pub(crate) fn run_elevated_program(
    program: &OsStr,
    args: &[OsString],
    label: &str,
    timeout: Option<Duration>,
) -> Result<CommandCapture, String> {
    let capture = ElevationCapture::new()?;
    let working_directory = env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    let inner_command = build_elevated_child_command(
        program,
        args,
        capture.stdout_path.as_os_str(),
        capture.stderr_path.as_os_str(),
    );
    write_elevated_command_file(&capture.command_path, &inner_command)?;
    let outcome = run_logged_elevated_command_file(
        capture.command_path.as_os_str(),
        &capture,
        label,
        &working_directory,
        timeout,
    )?;
    let visible_stderr = visible_elevated_stderr(&outcome.stderr_log);
    if !outcome.status.success() {
        emit_command_logs(
            &program.to_string_lossy(),
            &outcome.stdout_log,
            &visible_stderr,
        );
    }

    Ok(CommandCapture {
        stdout: outcome.stdout_log,
        stderr: visible_stderr,
        success: outcome.status.success(),
        status_code: outcome.status.code().unwrap_or(1),
        timed_out: false,
        timeout_seconds: None,
    })
}

pub(crate) struct ElevatedCommandOutcome {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout_log: String,
    pub(crate) stderr_log: String,
    pub(crate) status_log: String,
}

pub(crate) fn run_logged_elevated_command_file(
    command_path: &OsStr,
    capture: &ElevationCapture,
    label: &str,
    working_directory: &str,
    timeout: Option<Duration>,
) -> Result<ElevatedCommandOutcome, String> {
    let mut process = launch_elevated_command_file(
        command_path,
        capture.status_path.as_os_str(),
        working_directory,
        label,
    )?;

    let mut reporter = ProgressReporter::new();
    let started_at = Instant::now();
    loop {
        let current_label = latest_elevated_step(&read_capture_log(&capture.status_path))
            .unwrap_or_else(|| label.to_string());
        reporter.tick(&current_label)?;

        if process.wait(Duration::from_millis(150))?.is_some() {
            break;
        }
        if timeout.is_some_and(|limit| started_at.elapsed() >= limit) {
            process.terminate();
            reporter.finish(false, &current_label)?;
            return Err(format!(
                "elevated command timed out after {}s: {current_label}",
                timeout.map(|limit| limit.as_secs()).unwrap_or(0)
            ));
        }
    }

    let status = process
        .finish()
        .map_err(|error| format!("failed to collect elevated process result: {error}"))?;
    let stdout_log = read_capture_log(&capture.stdout_path);
    let stderr_log = read_capture_log(&capture.stderr_path);
    let status_log = read_capture_log(&capture.status_path);
    let final_label = latest_elevated_step(&status_log).unwrap_or_else(|| label.to_string());
    reporter.finish(status.success(), &final_label)?;

    Ok(ElevatedCommandOutcome {
        status,
        stdout_log,
        stderr_log,
        status_log,
    })
}

pub(crate) fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

pub(crate) fn escape_powershell_single_quoted_os(value: &OsStr) -> String {
    escape_powershell_single_quoted(&value.to_string_lossy())
}

pub(crate) fn windows_arg_escape(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\')
    {
        return value.to_string();
    }

    let mut escaped = String::from("\"");
    let mut backslashes = 0usize;
    for ch in value.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                escaped.push_str(&"\\".repeat(backslashes * 2 + 1));
                escaped.push('"');
                backslashes = 0;
            }
            _ => {
                if backslashes > 0 {
                    escaped.push_str(&"\\".repeat(backslashes));
                    backslashes = 0;
                }
                escaped.push(ch);
            }
        }
    }
    if backslashes > 0 {
        escaped.push_str(&"\\".repeat(backslashes * 2));
    }
    escaped.push('"');
    escaped
}

pub(crate) fn build_elevated_loader_command(command_path: &OsStr, status_path: &OsStr) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'; try {{ [scriptblock]::Create([System.IO.File]::ReadAllText('{}', [System.Text.Encoding]::UTF8)).Invoke() }} catch {{ $wawMessage = ($_.Exception.Message | Out-String).Trim(); if ([string]::IsNullOrWhiteSpace($wawMessage)) {{ $wawMessage = ($_ | Out-String).Trim() }}; Add-Content -LiteralPath '{}' -Value ('{}' + $wawMessage) -Encoding utf8; throw }}",
        escape_powershell_single_quoted_os(command_path),
        escape_powershell_single_quoted_os(status_path),
        ELEVATED_BOOTSTRAP_FAILURE_MARKER,
    )
}

#[cfg(windows)]
pub(crate) fn os_str_to_wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
pub(crate) struct ElevatedProcessHandle {
    pub(crate) handle: Handle,
}

#[cfg(windows)]
impl ElevatedProcessHandle {
    fn wait(&mut self, timeout: Duration) -> Result<Option<()>, String> {
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { WaitForSingleObject(self.handle, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(Some(())),
            WAIT_TIMEOUT => Ok(None),
            WAIT_FAILED => Err(std::io::Error::last_os_error().to_string()),
            code => Err(format!(
                "unexpected wait status while monitoring elevated process: {code}"
            )),
        }
    }

    fn terminate(&self) -> bool {
        unsafe { TerminateProcess(self.handle, 1) != 0 }
    }

    fn finish(self) -> Result<std::process::ExitStatus, String> {
        let mut exit_code = 1u32;
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut exit_code) };
        let close_result = unsafe { CloseHandle(self.handle) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if close_result == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(std::process::ExitStatus::from_raw(exit_code))
    }
}

#[cfg(not(windows))]
pub(crate) struct ElevatedProcessHandle;

#[cfg(not(windows))]
impl ElevatedProcessHandle {
    fn wait(&mut self, _timeout: Duration) -> Result<Option<()>, String> {
        Err("native elevation is only available on Windows".to_string())
    }

    fn finish(self) -> Result<std::process::ExitStatus, String> {
        Err("native elevation is only available on Windows".to_string())
    }
}

pub(crate) fn launch_elevated_command_file(
    command_path: &OsStr,
    status_path: &OsStr,
    working_directory: &str,
    label: &str,
) -> Result<ElevatedProcessHandle, String> {
    #[cfg(windows)]
    {
        let loader_command = build_elevated_loader_command(command_path, status_path);
        let parameters = [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            loader_command.as_str(),
        ]
        .into_iter()
        .map(windows_arg_escape)
        .collect::<Vec<_>>()
        .join(" ");
        let file = os_str_to_wide_null(OsStr::new("powershell.exe"));
        let verb = os_str_to_wide_null(OsStr::new("runas"));
        let parameters = os_str_to_wide_null(OsStr::new(&parameters));
        let directory = os_str_to_wide_null(OsStr::new(working_directory));
        let mut execute_info = ShellExecuteInfoW {
            cbSize: std::mem::size_of::<ShellExecuteInfoW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: std::ptr::null_mut(),
            lpVerb: verb.as_ptr(),
            lpFile: file.as_ptr(),
            lpParameters: parameters.as_ptr(),
            lpDirectory: directory.as_ptr(),
            nShow: 1,
            hInstApp: std::ptr::null_mut(),
            lpIDList: std::ptr::null_mut(),
            lpClass: std::ptr::null(),
            hkeyClass: std::ptr::null_mut(),
            dwHotKey: 0,
            Anonymous: unsafe { std::mem::zeroed() },
            hProcess: std::ptr::null_mut(),
        };
        let ok = unsafe { ShellExecuteExW(&mut execute_info) };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_CANCELLED) {
                return Err(
                    "administrator privileges were not granted. Confirm the Windows UAC prompt and try again."
                        .to_string(),
                );
            }
            return Err(format!(
                "failed to request administrator privileges for {label}: {error}"
            ));
        }
        Ok(ElevatedProcessHandle {
            handle: execute_info.hProcess,
        })
    }

    #[cfg(not(windows))]
    {
        let _ = (command_path, status_path, working_directory, label);
        Err("native elevation is only available on Windows".to_string())
    }
}

pub(crate) struct ElevationCapture {
    pub(crate) stdout_path: PathBuf,
    pub(crate) stderr_path: PathBuf,
    pub(crate) status_path: PathBuf,
    pub(crate) command_path: PathBuf,
}

impl ElevationCapture {
    pub(crate) fn new() -> Result<Self, String> {
        let base = unique_capture_base("waw-elevated")?;
        Ok(Self {
            stdout_path: base.with_extension("stdout.log"),
            stderr_path: base.with_extension("stderr.log"),
            status_path: base.with_extension("status.log"),
            command_path: base.with_extension("command.txt"),
        })
    }
}

impl Drop for ElevationCapture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
        let _ = fs::remove_file(&self.status_path);
        let _ = fs::remove_file(&self.command_path);
    }
}

pub(crate) fn build_elevated_child_command(
    program: &OsStr,
    args: &[OsString],
    stdout_path: &OsStr,
    stderr_path: &OsStr,
) -> String {
    let argument_list = args
        .iter()
        .map(|arg| format!("'{}'", escape_powershell_single_quoted_os(arg)))
        .collect::<Vec<_>>()
        .join(", ");
    let tolerated_exit_snippet = tolerated_elevated_exit_snippet_os(program, args);
    format!(
        "{}$proc = Start-Process -FilePath '{}' -ArgumentList @({}) -PassThru -Wait -RedirectStandardOutput '{}' -RedirectStandardError '{}'; $stdoutText = if (Test-Path -LiteralPath '{}') {{ Get-Content -LiteralPath '{}' -Raw }} else {{ '' }}; $stderrText = if (Test-Path -LiteralPath '{}') {{ Get-Content -LiteralPath '{}' -Raw }} else {{ '' }}; $code = if ($null -eq $proc.ExitCode) {{ 0 }} else {{ $proc.ExitCode }}; {} exit $code",
        powershell_utf8_setup(),
        escape_powershell_single_quoted_os(program),
        argument_list,
        escape_powershell_single_quoted_os(stdout_path),
        escape_powershell_single_quoted_os(stderr_path),
        escape_powershell_single_quoted_os(stdout_path),
        escape_powershell_single_quoted_os(stdout_path),
        escape_powershell_single_quoted_os(stderr_path),
        escape_powershell_single_quoted_os(stderr_path),
        tolerated_exit_snippet,
    )
}

pub(crate) fn build_elevated_batch_command(
    invocations: &[Invocation],
    stdout_path: &OsStr,
    stderr_path: &OsStr,
    status_path: &OsStr,
    continue_on_error: bool,
) -> String {
    let commands = invocations
        .iter()
        .filter(|invocation| !invocation.program.is_empty())
        .map(|invocation| {
            build_elevated_invocation_command(invocation, status_path, continue_on_error)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let batch_exit = if continue_on_error {
        "if ($script:wawBatchExitCode -ne 0) { exit $script:wawBatchExitCode } else { exit 0 }"
    } else {
        "exit 0"
    };
    format!(
        "{}$ErrorActionPreference = 'Stop'; $script:wawBatchExitCode = 0; & {{ {commands}; {batch_exit} }} 1>> '{}' 2>> '{}'",
        powershell_utf8_setup(),
        escape_powershell_single_quoted_os(stdout_path),
        escape_powershell_single_quoted_os(stderr_path),
    )
}

pub(crate) fn build_elevated_invocation_command(
    invocation: &Invocation,
    batch_stderr_path: &OsStr,
    continue_on_error: bool,
) -> String {
    let argument_list = invocation
        .args
        .iter()
        .map(|arg| format!("'{}'", escape_powershell_single_quoted(arg)))
        .collect::<Vec<_>>()
        .join(", ");
    let progress_label = invocation_progress_label(invocation);
    let tolerated_exit_snippet =
        tolerated_elevated_exit_snippet(&invocation.program, &invocation.args);
    let batch_stderr_path = escape_powershell_single_quoted_os(batch_stderr_path);
    let failure_tail = if continue_on_error {
        "if ($script:wawBatchExitCode -eq 0) { $script:wawBatchExitCode = $code }"
    } else {
        "exit $code"
    };
    format!(
        "Add-Content -LiteralPath '{}' -Value '{}{}' -Encoding utf8; [Console]::Error.WriteLine('{}{}'); $wawStdout = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [guid]::NewGuid().ToString() + '.stdout.log'); $wawStderr = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [guid]::NewGuid().ToString() + '.stderr.log'); try {{ $proc = Start-Process -FilePath '{}' -ArgumentList @({}) -PassThru -Wait -RedirectStandardOutput $wawStdout -RedirectStandardError $wawStderr; $stdoutText = if (Test-Path -LiteralPath $wawStdout) {{ Get-Content -LiteralPath $wawStdout -Raw }} else {{ '' }}; $stderrText = if (Test-Path -LiteralPath $wawStderr) {{ Get-Content -LiteralPath $wawStderr -Raw }} else {{ '' }}; if ($stdoutText.Length -gt 0) {{ [Console]::Out.Write($stdoutText) }}; if ($stderrText.Length -gt 0) {{ [Console]::Error.Write($stderrText) }}; $code = if ($null -eq $proc.ExitCode) {{ 0 }} else {{ $proc.ExitCode }}; {} if ($code -eq 0) {{ Add-Content -LiteralPath '{}' -Value '{}{}' -Encoding utf8; [Console]::Error.WriteLine('{}{}') }} else {{ Add-Content -LiteralPath '{}' -Value ('{}{}:' + $code) -Encoding utf8; [Console]::Error.WriteLine('{}{}:' + $code); {} }} }} finally {{ Remove-Item -LiteralPath $wawStdout -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $wawStderr -ErrorAction SilentlyContinue }}",
        batch_stderr_path,
        ELEVATED_STEP_MARKER,
        escape_powershell_single_quoted(&progress_label),
        ELEVATED_STEP_MARKER,
        escape_powershell_single_quoted(&progress_label),
        escape_powershell_single_quoted(&invocation.program),
        argument_list,
        tolerated_exit_snippet,
        batch_stderr_path,
        ELEVATED_SUCCESS_MARKER,
        escape_powershell_single_quoted(&progress_label),
        ELEVATED_SUCCESS_MARKER,
        escape_powershell_single_quoted(&progress_label),
        batch_stderr_path,
        ELEVATED_FAILURE_MARKER,
        escape_powershell_single_quoted(&progress_label),
        ELEVATED_FAILURE_MARKER,
        escape_powershell_single_quoted(&progress_label),
        failure_tail,
    )
}

pub(crate) fn tolerated_elevated_exit_snippet(program: &str, args: &[String]) -> &'static str {
    if is_winget_uninstall_command(program, args) {
        "if ($code -ne 0 -and [uint32]$code -eq 0x800401F5) { $code = 0 }; "
    } else if is_winget_upgrade_command(program, args) {
        "if ($code -ne 0 -and ([int]$code -eq -1978335189 -or [uint32]$code -eq 0x8A15002B -or ($stdoutText + \"`n\" + $stderrText) -match 'No applicable upgrade found' -or ($stdoutText + \"`n\" + $stderrText) -match 'does not apply to your system or requirements' -or ($stdoutText + \"`n\" + $stderrText) -match '找不到适用的升级' -or ($stdoutText + \"`n\" + $stderrText) -match '较新的包在配置的源中可用')) { $code = 0 }; "
    } else {
        ""
    }
}

pub(crate) fn tolerated_elevated_exit_snippet_os(
    program: &OsStr,
    args: &[OsString],
) -> &'static str {
    let program = program.to_string_lossy();
    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if is_winget_uninstall_command_os(&program, &args) {
        "if ($code -ne 0 -and [uint32]$code -eq 0x800401F5) { $code = 0 }; "
    } else if is_winget_upgrade_command_os(&program, &args) {
        "if ($code -ne 0 -and ([int]$code -eq -1978335189 -or [uint32]$code -eq 0x8A15002B -or ($stdoutText + \"`n\" + $stderrText) -match 'No applicable upgrade found' -or ($stdoutText + \"`n\" + $stderrText) -match 'does not apply to your system or requirements' -or ($stdoutText + \"`n\" + $stderrText) -match '找不到适用的升级' -or ($stdoutText + \"`n\" + $stderrText) -match '较新的包在配置的源中可用')) { $code = 0 }; "
    } else {
        ""
    }
}

pub(crate) fn is_winget_uninstall_command(program: &str, args: &[String]) -> bool {
    let program = program.replace('\\', "/").to_ascii_lowercase();
    program.contains("winget") && args.first().map(String::as_str) == Some("uninstall")
}

pub(crate) fn is_winget_uninstall_command_os(program: &str, args: &[String]) -> bool {
    let program = program.replace('\\', "/").to_ascii_lowercase();
    program.contains("winget") && args.first().map(String::as_str) == Some("uninstall")
}

pub(crate) fn is_winget_upgrade_command(program: &str, args: &[String]) -> bool {
    let program = program.replace('\\', "/").to_ascii_lowercase();
    program.contains("winget") && args.first().map(String::as_str) == Some("upgrade")
}

pub(crate) fn is_winget_upgrade_command_os(program: &str, args: &[String]) -> bool {
    let program = program.replace('\\', "/").to_ascii_lowercase();
    program.contains("winget") && args.first().map(String::as_str) == Some("upgrade")
}

pub(crate) fn is_tolerable_command_failure(
    invocation: &Invocation,
    capture: &CommandCapture,
) -> bool {
    if is_winget_upgrade_command(&invocation.program, &invocation.args) {
        if capture.status_code == WINGET_NO_APPLICABLE_UPGRADE_EXIT_CODE {
            return true;
        }
        let output = format!("{}\n{}", capture.stdout, capture.stderr);
        return winget_no_applicable_upgrade_message(&output);
    }
    false
}

pub(crate) fn winget_no_applicable_upgrade_message(output: &str) -> bool {
    let normalized = output.to_ascii_lowercase();
    normalized.contains("no applicable upgrade found")
        || normalized.contains("does not apply to your system or requirements")
        || output.contains("找不到适用的升级")
        || output.contains("较新的包在配置的源中可用")
}

pub(crate) fn write_elevated_command_file(path: &Path, content: &str) -> Result<(), String> {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(content.as_bytes());
    fs::write(path, bytes)
        .map_err(|error| format!("failed to write elevated command file: {error}"))
}

pub(crate) fn powershell_utf8_setup() -> &'static str {
    concat!(
        "$utf8NoBom = New-Object System.Text.UTF8Encoding($false); ",
        "[Console]::InputEncoding = $utf8NoBom; ",
        "[Console]::OutputEncoding = $utf8NoBom; ",
        "$OutputEncoding = $utf8NoBom; ",
        "chcp.com 65001 > $null; "
    )
}

pub(crate) fn latest_elevated_step(stderr_log: &str) -> Option<String> {
    stderr_log
        .lines()
        .filter_map(|line| line.trim().strip_prefix(ELEVATED_STEP_MARKER))
        .next_back()
        .map(ToString::to_string)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ElevatedBatchStepStatus {
    NotStarted,
    Started,
    Succeeded,
    Failed(i32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ElevatedBatchStepSummary {
    pub(crate) label: String,
    pub(crate) status: ElevatedBatchStepStatus,
}

pub(crate) fn summarize_elevated_batch(
    invocations: &[Invocation],
    stderr_log: &str,
) -> Vec<ElevatedBatchStepSummary> {
    let mut steps = invocations
        .iter()
        .filter(|invocation| !invocation.program.is_empty())
        .map(|invocation| ElevatedBatchStepSummary {
            label: invocation_progress_label(invocation),
            status: ElevatedBatchStepStatus::NotStarted,
        })
        .collect::<Vec<_>>();

    for line in stderr_log.lines() {
        let trimmed = line.trim();
        if let Some(label) = trimmed.strip_prefix(ELEVATED_STEP_MARKER) {
            if let Some(step) = steps.iter_mut().find(|step| {
                step.label == label && step.status == ElevatedBatchStepStatus::NotStarted
            }) {
                step.status = ElevatedBatchStepStatus::Started;
            }
            continue;
        }

        if let Some(label) = trimmed.strip_prefix(ELEVATED_SUCCESS_MARKER) {
            if let Some(step) = steps
                .iter_mut()
                .rev()
                .find(|step| step.label == label && step.status == ElevatedBatchStepStatus::Started)
            {
                step.status = ElevatedBatchStepStatus::Succeeded;
            }
            continue;
        }

        if let Some((label, exit_code)) = parse_elevated_failure_line(trimmed)
            && let Some(step) = steps
                .iter_mut()
                .rev()
                .find(|step| step.label == label && step.status == ElevatedBatchStepStatus::Started)
        {
            step.status = ElevatedBatchStepStatus::Failed(exit_code);
        }
    }

    steps
}

pub(crate) fn render_elevated_batch_summary(
    invocations: &[Invocation],
    stderr_log: &str,
) -> Option<String> {
    let steps = summarize_elevated_batch(invocations, stderr_log);
    if steps.is_empty() {
        return None;
    }

    let succeeded = steps
        .iter()
        .filter(|step| step.status == ElevatedBatchStepStatus::Succeeded)
        .map(|step| step.label.clone())
        .collect::<Vec<_>>();
    let failed = steps
        .iter()
        .filter_map(|step| match step.status {
            ElevatedBatchStepStatus::Failed(code) => Some((step.label.clone(), code)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let not_started = steps
        .iter()
        .filter(|step| step.status == ElevatedBatchStepStatus::NotStarted)
        .map(|step| step.label.clone())
        .collect::<Vec<_>>();
    let interrupted = steps
        .iter()
        .filter(|step| step.status == ElevatedBatchStepStatus::Started)
        .map(|step| step.label.clone())
        .collect::<Vec<_>>();

    if succeeded.is_empty() && failed.is_empty() && not_started.is_empty() && interrupted.is_empty()
    {
        return None;
    }

    let mut lines = vec!["Batch summary:".to_string()];
    if !succeeded.is_empty() {
        lines.push(format!("Succeeded ({}):", succeeded.len()));
        lines.extend(succeeded.into_iter().map(|label| format!("- {label}")));
    }
    if !failed.is_empty() {
        lines.push(format!("Failed ({}):", failed.len()));
        lines.extend(
            failed
                .into_iter()
                .map(|(label, code)| format!("- {label} (exit code {code})")),
        );
    }
    if !not_started.is_empty() {
        lines.push(format!("Not started ({}):", not_started.len()));
        lines.extend(not_started.into_iter().map(|label| format!("- {label}")));
    }
    if !interrupted.is_empty() {
        lines.push(format!("Interrupted ({}):", interrupted.len()));
        lines.extend(interrupted.into_iter().map(|label| format!("- {label}")));
    }

    Some(lines.join("\n"))
}

pub(crate) fn visible_elevated_stderr(stderr_log: &str) -> String {
    let lines = stderr_log
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with(ELEVATED_STEP_MARKER)
                && !trimmed.starts_with(ELEVATED_SUCCESS_MARKER)
                && !trimmed.starts_with(ELEVATED_FAILURE_MARKER)
                && !trimmed.starts_with(ELEVATED_BOOTSTRAP_FAILURE_MARKER)
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

pub(crate) fn format_elevated_batch_failure(exit_code: i32, stderr_log: &str) -> String {
    let failure_count = count_elevated_failures(stderr_log);
    if let Some(message) = parse_elevated_bootstrap_failure(stderr_log) {
        format!("elevated bootstrap failed: {message}")
    } else if failure_count > 1 {
        format!("{failure_count} backend commands failed; see batch summary above")
    } else if let Some((command, command_exit_code)) = parse_elevated_failure(stderr_log) {
        format!("backend command failed with exit code {command_exit_code}: {command}")
    } else {
        format!("one or more elevated backend commands failed with exit code {exit_code}")
    }
}

pub(crate) fn count_elevated_failures(stderr_log: &str) -> usize {
    stderr_log
        .lines()
        .filter(|line| parse_elevated_failure_line(line).is_some())
        .count()
}

pub(crate) fn parse_elevated_failure(stderr_log: &str) -> Option<(String, i32)> {
    stderr_log
        .lines()
        .filter_map(parse_elevated_failure_line)
        .next_back()
}

pub(crate) fn parse_elevated_failure_line(line: &str) -> Option<(String, i32)> {
    let payload = line.trim().strip_prefix(ELEVATED_FAILURE_MARKER)?;
    let (command, exit_code) = payload.rsplit_once(':')?;
    let exit_code = parse_windows_exit_code(exit_code)?;
    Some((command.to_string(), exit_code))
}

pub(crate) fn parse_elevated_bootstrap_failure(stderr_log: &str) -> Option<String> {
    stderr_log
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(ELEVATED_BOOTSTRAP_FAILURE_MARKER)
                .map(ToString::to_string)
        })
        .next_back()
}

pub(crate) fn parse_windows_exit_code(value: &str) -> Option<i32> {
    value
        .parse::<i32>()
        .ok()
        .or_else(|| value.parse::<u32>().ok().map(|code| code as i32))
}
