use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let unique = format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        );
        let path = env::temp_dir().join(unique);
        fs::create_dir_all(&path).expect("temporary test directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn update_without_backend_runs_all_enabled_available_backends() {
    let sandbox = TestDir::new("waw-e2e-update");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--dry-run",
            "update",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains(&format!(
        "> {} source update",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
    assert!(
        output
            .stdout
            .contains("npm does not expose a separate apt-get-style update step.")
    );
    assert!(
        output
            .stdout
            .contains("pip does not expose a separate apt-get-style update step.")
    );
}

#[test]
fn upgrade_without_backend_previews_updates_and_plans_selected_upgrades() {
    let sandbox = TestDir::new("waw-e2e-upgrade");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw_with_input(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--dry-run",
            "upgrade",
        ],
        "1-3\n",
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );

    assert!(output.stdout.contains("Git.Git"));
    assert!(output.stdout.contains("git-tool"));
    assert!(output.stdout.contains("pip"));
    assert!(output.stdout.contains("Select packages to upgrade"));
    assert!(output.stdout.contains(&format!(
        "> {} upgrade --id Git.Git --exact --accept-source-agreements --accept-package-agreements --silent --disable-interactivity",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
    assert!(output.stdout.contains("git-tool@latest"));
    assert!(output.stdout.contains("install pip --no-input --upgrade"));
}

#[test]
fn install_aggregates_search_results_and_plans_selected_install_by_default() {
    let sandbox = TestDir::new("waw-e2e-install-search");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw_with_input(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--dry-run",
            "install",
            "git",
        ],
        "1\n",
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("Searching winget for: git"));
    assert!(output.stdout.contains("Searching npm for: git"));
    assert!(output.stdout.contains("Select packages"));
    assert!(output.stdout.contains("Git.Git"));
    assert!(output.stdout.contains("git-tool"));
    assert!(output.stdout.contains(&format!(
        "> {} install --id Git.Git --exact --accept-source-agreements --accept-package-agreements --silent --disable-interactivity",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
}

#[test]
fn install_exact_bypasses_search_and_plans_direct_install() {
    let sandbox = TestDir::new("waw-e2e-install-exact");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--backend",
            "winget",
            "--dry-run",
            "install",
            "--exact",
            "Git.Git",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(!output.stdout.contains("Searching winget for:"));
    assert!(output.stdout.contains(&format!(
        "> {} install --id Git.Git --exact --accept-source-agreements --accept-package-agreements --silent --disable-interactivity",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
}

#[test]
fn remove_aggregates_installed_packages_and_plans_selected_uninstall_by_default() {
    let sandbox = TestDir::new("waw-e2e-remove-search");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw_with_input(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--dry-run",
            "remove",
            "git",
        ],
        "1\n",
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(
        output
            .stdout
            .contains("Searching installed packages in winget for: git")
    );
    assert!(
        output
            .stdout
            .contains("Searching installed packages in npm for: git")
    );
    assert!(output.stdout.contains("Select packages to remove"));
    assert!(output.stdout.contains("Git.Git"));
    assert!(output.stdout.contains("git-tool"));
    assert!(output.stdout.contains(&format!(
        "> {} uninstall --id Git.Git --exact --version 2.45 --silent --disable-interactivity",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
}

#[test]
fn remove_backend_winget_plans_uninstall_with_exact_mode() {
    let sandbox = TestDir::new("waw-e2e-remove-winget");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--backend",
            "winget",
            "--dry-run",
            "remove",
            "--exact",
            "Git.Git",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains(&format!(
        "> {} uninstall --id Git.Git --exact --silent --disable-interactivity",
        display_program(&command_path(sandbox.path(), "winget"))
    )));
}

#[test]
fn hold_and_unhold_render_backend_specific_plans() {
    let sandbox = TestDir::new("waw-e2e-hold");
    write_mock_commands(sandbox.path());
    let config_path = write_full_config(sandbox.path());

    let hold_output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--backend",
            "winget",
            "--dry-run",
            "hold",
            "Git.Git",
        ],
    );
    assert!(hold_output.status.success());
    assert!(hold_output.stdout.contains(&format!(
        "> {} pin add --id Git.Git --blocking --exact",
        display_program(&command_path(sandbox.path(), "winget"))
    )));

    let unhold_output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--backend",
            "choco",
            "--dry-run",
            "hold",
            "--off",
            "git",
        ],
    );
    assert!(unhold_output.status.success());
    assert!(unhold_output.stdout.contains(&format!(
        "> {} pin remove --name git",
        display_program(&command_path(sandbox.path(), "choco"))
    )));
}

#[test]
fn search_aggregates_results_and_sorts_them() {
    let sandbox = TestDir::new("waw-e2e-search");
    write_mock_commands(sandbox.path());
    let config_path = write_full_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "search",
            "git",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("Searching winget for: git"));
    assert!(output.stdout.contains("Searching npm for: git"));
    assert!(output.stdout.contains("Searching pip for: git"));

    let git_pos = output
        .stdout
        .find("Git [Git.Git]")
        .expect("winget result should render");
    let npm_pos = output
        .stdout
        .find("git-tool [git-tool]")
        .expect("npm result should render");
    assert!(git_pos < npm_pos);
}

#[test]
fn list_upgradable_aggregates_rows_from_multiple_backends() {
    let sandbox = TestDir::new("waw-e2e-list");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "list",
            "--upgradable",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("Backend"));
    assert!(output.stdout.contains("Current"));
    assert!(output.stdout.contains("Latest"));
    assert!(
        output
            .stdout
            .contains("Listing upgradable packages from winget")
    );
    assert!(output.stdout.contains("npm"));
    assert!(output.stdout.contains("git-tool"));
    assert!(output.stdout.contains("pip"));
}

#[test]
fn show_json_aggregates_results_from_multiple_backends() {
    let sandbox = TestDir::new("waw-e2e-show-json");
    write_mock_commands(sandbox.path());
    let config_path = write_config(sandbox.path());

    let output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--json",
            "show",
            "Git.Git",
        ],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("\"backend\":\"winget\""));
    assert!(output.stdout.contains("\"backend\":\"npm\""));
    assert!(output.stdout.contains("\"backend\":\"pip\""));
    assert!(output.stdout.contains("\"details\""));
    assert!(output.stdout.contains("\"success\":true"));
}

#[test]
fn backend_commands_update_config_and_print_bootstrap_plan() {
    let sandbox = TestDir::new("waw-e2e-backend");
    write_mock_commands(sandbox.path());
    let config_path = write_full_config(sandbox.path());

    let disable_output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "backend",
            "disable",
            "npm",
        ],
    );
    assert!(disable_output.status.success());
    let config_after_disable = fs::read_to_string(&config_path).expect("config should be readable");
    assert!(config_after_disable.contains("enable_npm = false"));

    let default_output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "backend",
            "default",
            "pip",
        ],
    );
    assert!(default_output.status.success());
    let config_after_default = fs::read_to_string(&config_path).expect("config should be readable");
    assert!(config_after_default.contains("backend = \"pip\""));

    fs::remove_file(command_path(sandbox.path(), "scoop")).expect("scoop mock should be removed");
    let install_output = run_waw(
        sandbox.path(),
        &[
            "--config",
            config_path.to_str().expect("config path should be utf-8"),
            "--dry-run",
            "backend",
            "install",
            "scoop",
        ],
    );
    assert!(install_output.status.success());
    assert!(
        install_output
            .stdout
            .contains("Automatic install is not implemented for scoop.")
    );
    assert!(install_output.stdout.contains("https://scoop.sh/"));
}

struct CommandOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_waw(sandbox: &Path, args: &[&str]) -> CommandOutput {
    run_waw_with_input(sandbox, args, "")
}

fn run_waw_with_input(sandbox: &Path, args: &[&str], input: &str) -> CommandOutput {
    let binary = env!("CARGO_BIN_EXE_waw");

    let mut child = Command::new(binary)
        .args(args)
        .env("WAW_WINGET_CMD", command_path(sandbox, "winget"))
        .env("WAW_SCOOP_CMD", command_path(sandbox, "scoop"))
        .env("WAW_CHOCO_CMD", command_path(sandbox, "choco"))
        .env("WAW_NPM_CMD", command_path(sandbox, "npm"))
        .env("WAW_PYTHON_CMD", command_path(sandbox, "python"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("waw binary should run");

    if !input.is_empty() {
        let mut stdin = child.stdin.take().expect("stdin should be piped");
        stdin
            .write_all(input.as_bytes())
            .expect("input should be written");
    }

    let output = child
        .wait_with_output()
        .expect("output should be collected");

    CommandOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn write_config(sandbox: &Path) -> PathBuf {
    let config_path = sandbox.join("config.toml");
    fs::write(
        &config_path,
        "\
enable_winget = true
enable_scoop = false
enable_choco = false
enable_npm = true
enable_pip = true
pip_user = false
",
    )
    .expect("config should be written");
    config_path
}

fn write_full_config(sandbox: &Path) -> PathBuf {
    let config_path = sandbox.join("config.toml");
    fs::write(
        &config_path,
        "\
enable_winget = true
enable_scoop = true
enable_choco = true
enable_npm = true
enable_pip = true
pip_user = false
",
    )
    .expect("config should be written");
    config_path
}

fn write_mock_commands(dir: &Path) {
    write_mock_command(dir, "winget", winget_script());
    write_mock_command(dir, "scoop", scoop_script());
    write_mock_command(dir, "choco", choco_script());
    write_mock_command(dir, "npm", npm_script());
    write_mock_command(dir, "python", python_script());
}

fn write_mock_command(dir: &Path, base_name: &str, body: &str) {
    let path = command_path(dir, base_name);

    fs::write(&path, body).expect("mock command should be written");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path)
            .expect("mock command metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("mock command should be executable");
    }
}

fn command_path(dir: &Path, base_name: &str) -> PathBuf {
    if cfg!(windows) {
        dir.join(format!("{base_name}.cmd"))
    } else {
        dir.join(base_name)
    }
}

fn display_program(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '*' | '/' | '\\')
    }) {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn winget_script() -> &'static str {
    if cfg!(windows) {
        "@echo off\r\nif \"%~1\"==\"--version\" exit /b 0\r\nif \"%~1\"==\"search\" (\r\n  echo Name Id Version Source\r\n  echo ----------------------------------------------\r\n  echo Git Git.Git 2.45 winget\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"list\" (\r\n  echo Name                         Id                           Version Source\r\n  echo ----------------------------------------------------------------------\r\n  echo Git                          Git.Git                      2.45    winget\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"upgrade\" (\r\n  if \"%~2\"==\"--id\" (\r\n    exit /b 0\r\n  )\r\n  echo Name                         Id                           Version Available Source\r\n  echo -------------------------------------------------------------------------------\r\n  echo Git                          Git.Git                      2.45    2.46      winget\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"show\" (\r\n  echo Name: Git\r\n  echo Version: 2.45\r\n  echo Publisher: Git Project\r\n  echo Homepage: https://git-scm.com/\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"search\" ]; then\n  printf 'Name Id Version Source\\n'\n  printf '%s\\n' '----------------------------------------------'\n  printf 'Git Git.Git 2.45 winget\\n'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ]; then\n  printf 'Name                         Id                           Version Source\\n'\n  printf '%s\\n' '----------------------------------------------------------------------'\n  printf 'Git                          Git.Git                      2.45    winget\\n'\n  exit 0\nfi\nif [ \"$1\" = \"upgrade\" ]; then\n  if [ \"$2\" = \"--id\" ]; then\n    exit 0\n  fi\n  printf 'Name                         Id                           Version Available Source\\n'\n  printf '%s\\n' '-------------------------------------------------------------------------------'\n  printf 'Git                          Git.Git                      2.45    2.46      winget\\n'\n  exit 0\nfi\nif [ \"$1\" = \"show\" ]; then\n  printf 'Name: Git\\nVersion: 2.45\\nPublisher: Git Project\\nHomepage: https://git-scm.com/\\n'\n  exit 0\nfi\nexit 0\n"
    }
}

fn scoop_script() -> &'static str {
    if cfg!(windows) {
        "@echo off\r\nif \"%~1\"==\"--version\" exit /b 0\r\nif \"%~1\"==\"search\" (\r\n  echo 'main' bucket:\r\n  echo git 2.45\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"status\" (\r\n  echo git 2.45 -^> 2.46\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"info\" (\r\n  echo Name: git\r\n  echo Version: 2.45\r\n  echo Homepage: https://git-scm.com/\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"search\" ]; then\n  printf \"'main' bucket:\\ngit 2.45\\n\"\n  exit 0\nfi\nif [ \"$1\" = \"status\" ]; then\n  printf 'git 2.45 -> 2.46\\n'\n  exit 0\nfi\nif [ \"$1\" = \"info\" ]; then\n  printf 'Name: git\\nVersion: 2.45\\nHomepage: https://git-scm.com/\\n'\n  exit 0\nfi\nexit 0\n"
    }
}

fn choco_script() -> &'static str {
    if cfg!(windows) {
        "@echo off\r\nif \"%~1\"==\"-v\" exit /b 0\r\nif \"%~1\"==\"search\" (\r\n  echo git^|2.45\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"outdated\" (\r\n  echo git^|2.45^|2.46\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"info\" (\r\n  echo Title: git\r\n  echo Version: 2.45\r\n  echo Project Source URL: https://git-scm.com/\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"-v\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"search\" ]; then\n  printf 'git|2.45\\n'\n  exit 0\nfi\nif [ \"$1\" = \"outdated\" ]; then\n  printf 'git|2.45|2.46\\n'\n  exit 0\nfi\nif [ \"$1\" = \"info\" ]; then\n  printf 'Title: git\\nVersion: 2.45\\nProject Source URL: https://git-scm.com/\\n'\n  exit 0\nfi\nexit 0\n"
    }
}

fn npm_script() -> &'static str {
    if cfg!(windows) {
        "@echo off\r\nif \"%~1\"==\"--version\" exit /b 0\r\nif \"%~1\"==\"search\" (\r\n  echo [{\"name\":\"git-tool\",\"version\":\"1.0.0\"}]\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"list\" (\r\n  echo {\r\n  echo   \"dependencies\": {\r\n  echo     \"git-tool\": {\r\n  echo       \"version\": \"1.0.0\"\r\n  echo     }\r\n  echo   }\r\n  echo }\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"outdated\" (\r\n  echo {\r\n  echo   \"git-tool\": {\r\n  echo     \"current\": \"1.0.0\",\r\n  echo     \"latest\": \"1.1.0\"\r\n  echo   }\r\n  echo }\r\n  exit /b 1\r\n)\r\nif \"%~1\"==\"view\" (\r\n  echo git-tool@1.0.0 ^| MIT\r\n  echo Handy git helper\r\n  echo https://example.com/git-tool\r\n  echo dependencies:\r\n  echo left-pad: 1.3.0\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"search\" ]; then\n  printf '[{\"name\":\"git-tool\",\"version\":\"1.0.0\"}]'\n  exit 0\nfi\nif [ \"$1\" = \"list\" ]; then\n  printf '{\\n  \"dependencies\": {\\n    \"git-tool\": {\\n      \"version\": \"1.0.0\"\\n    }\\n  }\\n}'\n  exit 0\nfi\nif [ \"$1\" = \"outdated\" ]; then\n  printf '{\\n  \"git-tool\": {\\n    \"current\": \"1.0.0\",\\n    \"latest\": \"1.1.0\"\\n  }\\n}'\n  exit 1\nfi\nif [ \"$1\" = \"view\" ]; then\n  printf 'git-tool@1.0.0 | MIT\\nHandy git helper\\nhttps://example.com/git-tool\\ndependencies:\\nleft-pad: 1.3.0\\n'\n  exit 0\nfi\nexit 0\n"
    }
}

fn python_script() -> &'static str {
    if cfg!(windows) {
        "@echo off\r\nif \"%~1\"==\"-m\" if \"%~2\"==\"pip\" if \"%~3\"==\"--version\" exit /b 0\r\nif \"%~1\"==\"-c\" (\r\n  echo GitPython\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"-m\" if \"%~2\"==\"pip\" if \"%~3\"==\"list\" if \"%~4\"==\"--format=json\" (\r\n  echo [{\"name\":\"GitPython\",\"version\":\"3.1.43\"}]\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"-m\" if \"%~2\"==\"pip\" if \"%~3\"==\"list\" if \"%~4\"==\"--outdated\" if \"%~5\"==\"--format=json\" (\r\n  echo [{\"name\":\"pip\",\"version\":\"26.0\",\"latest_version\":\"26.0.1\"}]\r\n  exit /b 0\r\n)\r\nif \"%~1\"==\"-m\" if \"%~2\"==\"pip\" if \"%~3\"==\"show\" (\r\n  echo Name: Git.Git\r\n  echo Version: 2.45\r\n  echo Summary: Git package mirror\r\n  echo Home-page: https://pypi.org/project/git/\r\n  echo Requires: requests\r\n  exit /b 0\r\n)\r\nexit /b 0\r\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pip\" ] && [ \"$3\" = \"--version\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"-c\" ]; then\n  printf 'GitPython\\n'\n  exit 0\nfi\nif [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pip\" ] && [ \"$3\" = \"list\" ] && [ \"$4\" = \"--format=json\" ]; then\n  printf '[{\"name\":\"GitPython\",\"version\":\"3.1.43\"}]'\n  exit 0\nfi\nif [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pip\" ] && [ \"$3\" = \"list\" ] && [ \"$4\" = \"--outdated\" ] && [ \"$5\" = \"--format=json\" ]; then\n  printf '[{\"name\":\"pip\",\"version\":\"26.0\",\"latest_version\":\"26.0.1\"}]'\n  exit 0\nfi\nif [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pip\" ] && [ \"$3\" = \"show\" ]; then\n  printf 'Name: Git.Git\\nVersion: 2.45\\nSummary: Git package mirror\\nHome-page: https://pypi.org/project/git/\\nRequires: requests\\n'\n  exit 0\nfi\nexit 0\n"
    }
}
