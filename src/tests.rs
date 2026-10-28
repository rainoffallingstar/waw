//! Unit tests for the crate root. Kept out of `main.rs` so the module
//! files stay readable.

use crate::*;

#[test]
fn parses_winget_localized_show_fields() {
    let output = concat!(
        "已找到 Git [Git.Git]\r\n",
        "版本: 2.56.0.3\r\n",
        "发布者: The Git Development Community\r\n",
        "主页: https://gitforwindows.org/\r\n",
        "许可证: GPL-2.0\r\n",
        "描述:\r\n",
        "  Git is a distributed version control system.\r\n",
        "标记：\r\n",
        "  git\r\n",
        "  版本控制系统\r\n",
    );

    let details = parse_key_value_show_details(output).expect("should parse");
    assert_eq!(details.name, "Git");
    assert_eq!(details.version, "2.56.0.3");
    assert_eq!(details.license.as_deref(), Some("GPL-2.0"));
    assert_eq!(
        details.author.as_deref(),
        Some("The Git Development Community")
    );
    assert_eq!(
        details.homepage.as_deref(),
        Some("https://gitforwindows.org/")
    );
    assert!(
        details
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("version control"),
        "summary should absorb indented continuation: {:?}",
        details.summary
    );
}

#[test]
fn parses_full_width_colon_fields() {
    let output = "名称：demo\n版本：1.2.3\n";
    let fields = parse_colon_fields(output);
    assert_eq!(fields.len(), 2);
    assert_eq!(
        find_field_value(&fields, NAME_ALIASES).as_deref(),
        Some("demo")
    );
    assert_eq!(
        find_field_value(&fields, VERSION_ALIASES).as_deref(),
        Some("1.2.3")
    );
}

#[test]
fn parses_english_show_fields_without_regression() {
    let output = "\
Name: git\n\
Version: 2.53.0.3\n\
Source: main\n\
License: GPL-2.0-only\n";
    let details = parse_key_value_show_details(output).expect("should parse");
    assert_eq!(details.name, "git");
    assert_eq!(details.version, "2.53.0.3");
    assert_eq!(details.license.as_deref(), Some("GPL-2.0-only"));
}

#[test]
fn parses_choco_show_details() {
    let output = "\
Chocolatey v2.4.3\r\n\
rustup.install 1.27.1 [Approved] Downloads cached for licensed users\r\n\
 Title: rustup (Install) | Published: 2024/8/30\r\n\
 Package url https://community.chocolatey.org/packages/rustup.install/1.27.1\r\n\
  Chocolatey Package Source: https://github.com/camilohe/rustup.install\r\n\
 Tags: rustup rust\r\n\
 Software Site: https://rustup.rs/\r\n\
 Summary: rustup: the Rust toolchain installer\r\n";

    let details = parse_choco_show_details(output).expect("should parse");
    assert_eq!(details.name, "rustup");
    assert_eq!(details.version, "1.27.1");
    assert_eq!(details.homepage.as_deref(), Some("https://rustup.rs/"));
    assert_eq!(
        details.summary.as_deref(),
        Some("rustup: the Rust toolchain installer")
    );
    assert_eq!(details.keywords, vec!["rustup", "rust"]);
}

#[test]
fn winget_found_header_supplies_name() {
    let output = "已找到 7-Zip [7zip.7zip]\n版本: 26.00\n";
    let details = parse_key_value_show_details(output).expect("should parse");
    assert_eq!(details.name, "7-Zip");
    assert_eq!(details.version, "26.00");
}

#[test]
fn list_json_is_valid_and_complete() {
    let rows = vec![PackageListEntry {
        backend: Backend::Winget,
        name: "123云盘".to_string(),
        package_id: "123.123pan".to_string(),
        current_version: "3.1.5.0".to_string(),
        available_version: Some("3.2.8.0".to_string()),
    }];
    let json = render_list_results_json(&rows, true);
    assert!(json.contains("\"backend\":\"winget\""));
    assert!(json.contains("123云盘"));
    assert!(json.contains("\"available_version\":\"3.2.8.0\""));
    assert!(json.starts_with("[\n"));
}

#[test]
fn search_json_renders_nulls() {
    let candidates = vec![SearchCandidate {
        backend: Backend::Npm,
        label: "typescript".to_string(),
        install_id: "typescript".to_string(),
        version: None,
        source: None,
    }];
    let json = render_search_results_json(&candidates);
    assert!(json.contains("\"version\":null"));
    assert!(json.contains("\"source\":null"));
}

#[test]
fn timeout_from_seconds_treats_zero_as_unlimited() {
    assert!(timeout_from_seconds(0).is_none());
    assert_eq!(
        timeout_from_seconds(DEFAULT_COMMAND_TIMEOUT_SECONDS),
        Some(Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECONDS))
    );
    assert_eq!(timeout_from_seconds(5), Some(Duration::from_secs(5)));
}

#[test]
fn timeout_failure_message_is_distinct_from_exit_code_failure() {
    let invocation = Invocation::owned("tool", vec!["list".to_string()]);
    let capture = CommandCapture {
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        status_code: 1,
        timed_out: true,
        timeout_seconds: None,
    };
    let message = render_command_failure(&invocation, &capture);
    assert!(
        message.contains("timed out"),
        "timed-out command should say so, got: {message}"
    );
    assert!(!message.contains("exit code"));
}

// Only the capture-file tests below create files in the temp directory, and
// they assert on that directory's contents. Running them concurrently makes
// those assertions observe each other's files, so they take this lock.
static CAPTURE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn capture_test_guard() -> std::sync::MutexGuard<'static, ()> {
    CAPTURE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn count_capture_files(prefix: &str) -> usize {
    fs::read_dir(env::temp_dir())
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn command_capture_files_are_removable_by_drop() {
    let _guard = capture_test_guard();
    let capture = CommandLogCapture::new().expect("capture should be created");
    let stdout_path = capture.stdout_path.clone();
    let stderr_path = capture.stderr_path.clone();

    fs::write(&stdout_path, b"out").expect("stdout should be writable");
    fs::write(&stderr_path, b"err").expect("stderr should be writable");
    assert!(stdout_path.exists());
    assert!(stderr_path.exists());

    drop(capture);

    assert!(
        !stdout_path.exists(),
        "stdout capture should be removed on drop"
    );
    assert!(
        !stderr_path.exists(),
        "stderr capture should be removed on drop"
    );
}

#[test]
fn elevation_capture_files_are_removable_by_drop() {
    let _guard = capture_test_guard();
    let capture = ElevationCapture::new().expect("capture should be created");
    let paths = [
        capture.stdout_path.clone(),
        capture.stderr_path.clone(),
        capture.status_path.clone(),
        capture.command_path.clone(),
    ];
    for path in &paths {
        fs::write(path, b"x").expect("capture file should be writable");
        assert!(path.exists());
    }

    drop(capture);

    for path in &paths {
        assert!(
            !path.exists(),
            "{} should be removed on drop",
            path.display()
        );
    }
}

#[test]
fn capture_names_are_unique_under_rapid_creation() {
    let _guard = capture_test_guard();
    // Windows SystemTime granularity is roughly 15 ms, so this guards the
    // parallel backend path against capture files clobbering each other.
    let mut seen = std::collections::HashSet::new();
    for _ in 0..500 {
        let base = unique_capture_base("waw-uniqueness").expect("base should build");
        assert!(
            seen.insert(base.clone()),
            "duplicate capture base generated: {}",
            base.display()
        );
    }
}

#[test]
fn failing_command_still_removes_capture_files() {
    let _guard = capture_test_guard();
    let before = count_capture_files("waw-command-");
    let invocation = Invocation::owned(
        "waw-definitely-not-a-real-program-7f3a",
        vec!["list".to_string()],
    );

    let result =
        run_logged_command_capture_with_label(&invocation, "probe", false, None, |_| false);
    assert!(result.is_err(), "launching a missing program must fail");

    let after = count_capture_files("waw-command-");
    assert_eq!(
        before, after,
        "a failed launch must not leave capture files behind (before={before}, after={after})"
    );
}

#[test]
fn parses_minimal_json_values() {
    let value = parse_json(r#"{"a":1,"b":[true,false,null],"c":"x","d":{"e":2}}"#)
        .expect("json should parse");
    assert_eq!(value.get("c").and_then(JsonValue::as_str), Some("x"));
    assert_eq!(
        value.get("b").and_then(JsonValue::as_array).map(<[_]>::len),
        Some(3)
    );
    assert_eq!(
        value.get("d").and_then(|inner| inner.get("e")),
        Some(&JsonValue::Number(2.0))
    );
}

#[test]
fn json_parser_rejects_trailing_garbage() {
    assert!(parse_json(r#"{"a":1} trailing"#).is_err());
    assert!(parse_json(r#"{"a":}"#).is_err());
    assert!(parse_json("").is_err());
}

#[test]
fn json_parser_handles_escapes_and_unicode() {
    let value = parse_json(r#"{"k":"a\"b\\c\u4e91"}"#).expect("json should parse");
    assert_eq!(
        value.get("k").and_then(JsonValue::as_str),
        Some("a\"b\\c云")
    );
}

#[test]
fn winget_resource_ids_are_not_installable() {
    assert!(!is_installable_package_id(
        Backend::Winget,
        r"ARP\Machine\X64\74402CAB-65A4-4F40-8D63-02A6DB3FB923_is1"
    ));
    assert!(!is_installable_package_id(
        Backend::Winget,
        r"MSIX\MFSyncCloudShell_1.0.0.0_x64__f7bd3qkyghp9r"
    ));
    assert!(is_installable_package_id(Backend::Winget, "123.123pan"));
    assert!(is_installable_package_id(Backend::Scoop, "git"));
    assert!(is_installable_package_id(Backend::Npm, "@scope/pkg"));
}

#[test]
fn manifest_round_trips_through_render_and_parse() {
    let packages = vec![
        ManifestPackage {
            backend: Backend::Winget,
            id: "123.123pan".to_string(),
            name: "123云盘".to_string(),
            version: Some("3.1.5.0".to_string()),
        },
        ManifestPackage {
            backend: Backend::Scoop,
            id: "git".to_string(),
            name: "git".to_string(),
            version: Some("2.53.0.3".to_string()),
        },
    ];

    let rendered = render_manifest_json(&packages);
    let parsed = parse_manifest(&rendered).expect("manifest should parse");
    assert_eq!(parsed, packages);
}

#[test]
fn manifest_accepts_winget_export_format() {
    let winget_export = r#"{
  "$schema": "https://aka.ms/winget-packages.schema.2.0.json",
  "CreationDate": "2026-10-27T17:28:06.559-00:00",
  "Sources": [
    {"Packages": [
      {"PackageIdentifier": "7zip.7zip"},
      {"PackageIdentifier": "Git.Git"}
    ], "SourceDetails": {"Name": "winget"}}
  ]
}"#;

    let parsed = parse_manifest(winget_export).expect("winget export should parse");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].backend, Backend::Winget);
    assert_eq!(parsed[0].id, "7zip.7zip");
    assert_eq!(parsed[1].id, "Git.Git");
}

#[test]
fn manifest_rejects_unknown_format() {
    assert!(parse_manifest(r#"{"unexpected": true}"#).is_err());
    assert!(parse_manifest("not json").is_err());
}

#[test]
fn npm_list_keeps_entries_without_version() {
    // npm emits dependency entries with no "version" key for packages it
    // treats as overridden or partially installed. A line-based state
    // machine used to swallow everything after the first such entry.
    let output = r#"{
  "name": "npm",
  "dependencies": {
    "@scope/alpha": { "version": "1.0.0", "overridden": false },
    "code-server": { "overridden": false },
    "cowsay": { "version": "1.6.0", "overridden": false }
  }
}"#;

    let entries = parse_npm_list_entries(output).expect("should parse");
    let names: Vec<&str> = entries.iter().map(|e| e.package_id.as_str()).collect();
    assert_eq!(names, vec!["@scope/alpha", "code-server", "cowsay"]);
    assert_eq!(entries[0].current_version, "1.0.0");
    // Missing version is reported as "-", not dropped.
    assert_eq!(entries[1].current_version, "-");
    assert_eq!(entries[2].current_version, "1.6.0");
}

#[test]
fn npm_list_handles_empty_dependencies() {
    let entries =
        parse_npm_list_entries(r#"{"name":"npm","dependencies":{}}"#).expect("should parse");
    assert!(entries.is_empty());
    assert!(parse_npm_list_entries("{}").expect("ok").is_empty());
    assert!(parse_npm_list_entries("").expect("ok").is_empty());
}

#[test]
fn npm_list_rejects_non_json() {
    assert!(parse_npm_list_entries("Listing packages...").is_none());
}

#[test]
fn choco_list_skips_retry_noise_before_data() {
    // Real Chocolatey 2.7 output when it cannot replace choco.exe.old:
    // the retry/warning text goes to stdout ahead of the package rows.
    let output = "\
This is try 1/3. Retrying after 300 milliseconds.\r\n\
 Error converted to warning:\r\n\
 对路径“C:\\ProgramData\\chocolatey\\choco.exe.old”的访问被拒绝。\r\n\
This is try 2/3. Retrying after 400 milliseconds.\r\n\
 Error converted to warning:\r\n\
 对路径“C:\\ProgramData\\chocolatey\\choco.exe.old”的访问被拒绝。\r\n\
Maximum tries of 3 reached. Throwing error.\r\n\
chocolatey|2.7.4\r\n\
less|679.0.0\r\n\
rustup.install|1.27.1\r\n";

    let entries = parse_choco_list_entries(output).expect("should parse");
    assert_eq!(
        entries.len(),
        3,
        "noise must not hide packages: {entries:?}"
    );
    assert_eq!(entries[0].package_id, "chocolatey");
    assert_eq!(entries[0].current_version, "2.7.4");
    assert_eq!(entries[1].package_id, "less");
    assert_eq!(entries[2].package_id, "rustup.install");
}

#[test]
fn choco_list_still_parses_clean_output() {
    let entries = parse_choco_list_entries("less|679.0.0\n").expect("should parse");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].package_id, "less");
}

#[test]
fn choco_list_empty_output_is_not_an_error() {
    let entries = parse_choco_list_entries("").expect("empty is fine");
    assert!(entries.is_empty());
}

#[test]
fn choco_upgrade_skips_noise_lines() {
    let output = "\
This is try 1/3. Retrying after 300 milliseconds.\r\n\
Maximum tries of 3 reached. Throwing error.\r\n\
less|679.0.0|700.0.0\r\n";
    let entries = parse_choco_upgrade_entries(output).expect("should parse");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].package_id, "less");
    assert_eq!(entries[0].available_version.as_deref(), Some("700.0.0"));
}

#[test]
fn all_backends_failed_only_when_every_backend_failed() {
    assert!(all_backends_failed("list", 3, &[]).is_none());
    assert!(all_backends_failed("list", 3, &["a: x".to_string()]).is_none());
    assert!(all_backends_failed("list", 3, &["a: x".to_string(), "b: y".to_string()]).is_none());

    let error = all_backends_failed("list", 2, &["a: boom".to_string(), "b: bang".to_string()])
        .expect("all failed should produce an error");
    assert!(error.contains("all 2 backend(s) failed"), "{error}");
    assert!(error.contains("a: boom"), "{error}");
    assert!(error.contains("b: bang"), "{error}");

    // Defensive: no backends attempted must not claim failure.
    assert!(all_backends_failed("list", 0, &[]).is_none());
}

#[test]
fn only_chocolatey_requires_elevation() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    assert!(Backend::Chocolatey.requires_elevation_for_mutation(&runtime));
    assert!(!Backend::Winget.requires_elevation_for_mutation(&runtime));
    assert!(!Backend::Scoop.requires_elevation_for_mutation(&runtime));
    assert!(!Backend::Npm.requires_elevation_for_mutation(&runtime));
    // pip honours `pip_user`; the default config sets it true.
    assert!(!Backend::Pip.requires_elevation_for_mutation(&runtime));

    let global_pip = Config {
        pip_user: false,
        ..Config::default()
    };
    let global_runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &global_pip,
    };
    assert!(Backend::Pip.requires_elevation_for_mutation(&global_runtime));
}

#[test]
fn parses_package_spec_forms() {
    let plain = PackageSpec::parse("git");
    assert_eq!(plain.name, "git");
    assert_eq!(plain.version, None);

    let at = PackageSpec::parse("git@2.43.0");
    assert_eq!(at.name, "git");
    assert_eq!(at.version.as_deref(), Some("2.43.0"));

    let pip = PackageSpec::parse("requests==2.31.0");
    assert_eq!(pip.name, "requests");
    assert_eq!(pip.version.as_deref(), Some("2.31.0"));

    let scoped = PackageSpec::parse("@scope/name");
    assert_eq!(scoped.name, "@scope/name");
    assert_eq!(scoped.version, None);

    let scoped_versioned = PackageSpec::parse("@scope/name@1.2.3");
    assert_eq!(scoped_versioned.name, "@scope/name");
    assert_eq!(scoped_versioned.version.as_deref(), Some("1.2.3"));

    // A version containing a dot-less scope separator must not be split
    // into garbage.
    let pip_extras = PackageSpec::parse("pkg[extra]==1.0");
    assert_eq!(pip_extras.name, "pkg[extra]");
    assert_eq!(pip_extras.version.as_deref(), Some("1.0"));
}

#[test]
fn winget_install_pins_version_with_flag() {
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &Config::default(),
    };
    let specs = vec![PackageSpec::parse("Git.Git@2.43.0")];
    let invocations = Backend::Winget.plan_install(&specs, &runtime);
    assert_eq!(invocations.len(), 1);
    let rendered = invocations[0].render_for_display();
    assert!(rendered.contains("--id Git.Git"), "{rendered}");
    assert!(rendered.contains("--version 2.43.0"), "{rendered}");
    assert!(rendered.contains("--exact"), "{rendered}");
}

#[test]
fn npm_install_pins_version_in_argument() {
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &Config::default(),
    };
    let specs = vec![PackageSpec::parse("@types/node@20.11.0")];
    let invocations = Backend::Npm.plan_install(&specs, &runtime);
    assert_eq!(invocations.len(), 1);
    let rendered = invocations[0].render_for_display();
    assert!(rendered.contains("@types/node@20.11.0"), "{rendered}");
}

#[test]
fn pip_install_pins_version_with_double_equals() {
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &Config::default(),
    };
    let specs = vec![PackageSpec::parse("requests@2.31.0")];
    let invocations = Backend::Pip.plan_install(&specs, &runtime);
    let rendered = invocations[0].render_for_display();
    assert!(rendered.contains("requests==2.31.0"), "{rendered}");
}

#[test]
fn choco_install_pins_version_with_flag() {
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &Config::default(),
    };
    let specs = vec![PackageSpec::parse("nodejs.install@0.10.35")];
    let invocations = Backend::Chocolatey.plan_install(&specs, &runtime);
    let rendered = invocations[0].render_for_display();
    assert!(rendered.contains("--version 0.10.35"), "{rendered}");
}

#[test]
fn scoop_install_pins_version_in_argument() {
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &Config::default(),
    };
    let specs = vec![PackageSpec::parse("gh@2.7.0")];
    let invocations = Backend::Scoop.plan_install(&specs, &runtime);
    let rendered = invocations[0].render_for_display();
    // The `@` makes the renderer quote the argument; the target still
    // reaches scoop verbatim.
    assert!(rendered.contains("install"), "{rendered}");
    assert!(rendered.contains("gh@2.7.0"), "{rendered}");
}

#[test]
fn pip_results_are_not_reordered_by_the_generic_ranker() {
    // The prober ranks by publication date, so it puts the maintained
    // beautifulsoup4 ahead of the abandoned BeautifulSoup. The generic
    // prefix ranker would undo that because "BeautifulSoup" is an exact
    // match. Sorting must be a no-op for pure-pip result sets.
    let candidates = vec![
        SearchCandidate {
            backend: Backend::Pip,
            label: "beautifulsoup4 - Screen-scraping library".to_string(),
            install_id: "beautifulsoup4".to_string(),
            version: Some("4.15.0".to_string()),
            source: Some("pypi".to_string()),
        },
        SearchCandidate {
            backend: Backend::Pip,
            label: "BeautifulSoup - Screen-scraping library".to_string(),
            install_id: "BeautifulSoup".to_string(),
            version: Some("3.2.2".to_string()),
            source: Some("pypi".to_string()),
        },
    ];

    let sorted = sort_search_candidates(candidates.clone(), "beautifulsoup", &[Backend::Pip]);
    assert_eq!(
        sorted[0].install_id, "beautifulsoup4",
        "the prober's date-based order must survive"
    );
}

#[test]
fn generic_ranking_still_applies_to_mixed_backends() {
    let candidates = vec![
        SearchCandidate {
            backend: Backend::Npm,
            label: "eslint-plugin-git".to_string(),
            install_id: "eslint-plugin-git".to_string(),
            version: None,
            source: None,
        },
        SearchCandidate {
            backend: Backend::Npm,
            label: "git".to_string(),
            install_id: "git".to_string(),
            version: None,
            source: None,
        },
    ];
    let sorted = sort_search_candidates(candidates, "git", &[Backend::Npm]);
    assert_eq!(sorted[0].install_id, "git", "exact match still leads");
}

#[test]
fn pip_search_parses_probe_json() {
    let output = r#"[
            {"name": "requests", "version": "2.34.2", "summary": "Python HTTP for Humans."},
            {"name": "requests-api", "version": "1.1.5", "summary": "HTTP requests library."}
        ]"#;
    let candidates = parse_pip_search_candidates(output);
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].install_id, "requests");
    assert_eq!(candidates[0].version.as_deref(), Some("2.34.2"));
    assert_eq!(candidates[0].label, "requests - Python HTTP for Humans.");
}

#[test]
fn pip_search_falls_back_to_bare_names() {
    let candidates = parse_pip_search_candidates("GitPython\nrequests\n");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].install_id, "GitPython");
    assert_eq!(candidates[0].version, None);
}

#[test]
fn pip_search_handles_empty_and_missing_summary() {
    assert!(parse_pip_search_candidates("[]").is_empty());
    assert!(parse_pip_search_candidates("").is_empty());
    let candidates =
        parse_pip_search_candidates(r#"[{"name":"solo","version":"1.0","summary":null}]"#);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].label, "solo");
}

#[test]
fn pip_search_script_only_downloads_the_index_on_demand() {
    // The default path must stay probe-only; the full index is opt-in via
    // --deep and gated behind a cache check.
    let (default_part, deep_part) = PIP_SEARCH_SCRIPT
        .split_once("--deep")
        .expect("script should distinguish deep mode");
    assert!(
        !default_part.contains("pypi.org/simple"),
        "the probe path must not fetch the simple index"
    );
    assert!(
        default_part.contains("/json"),
        "the probe path should use the per-project JSON API"
    );
    assert!(
        deep_part.contains("pypi.org/simple"),
        "the deep path is what fetches the index"
    );
    assert!(
        deep_part.contains("If-None-Match"),
        "the deep path must revalidate with the cache ETag"
    );
}

#[test]
fn global_flags_split_before_and_after_subcommand() {
    let parse = |args: &[&str]| {
        Cli::parse(args.iter().map(|value| value.to_string()))
            .unwrap_or_else(|error| panic!("{args:?} should parse: {error}"))
    };

    let before = parse(&["--json", "list"]);
    assert!(before.json);

    let after = parse(&["list", "--json"]);
    assert!(after.json, "global flag after the command must be honoured");

    let mixed = parse(&["list", "--upgradable", "--json"]);
    assert!(mixed.json);
    assert_eq!(mixed.command, Subcommand::List { upgradable: true });

    let dry = parse(&["install", "--exact", "git", "--dry-run"]);
    assert!(dry.dry_run);
}

#[test]
fn global_flag_split_respects_value_taking_options() {
    // `--file` takes a value; the flag after it is a path, not a global.
    let (remaining, globals) = split_global_flags(&[
        "--file".to_string(),
        "--json".to_string(),
        "--dry-run".to_string(),
    ]);
    assert_eq!(remaining, vec!["--file".to_string(), "--json".to_string()]);
    assert_eq!(globals, vec!["--dry-run".to_string()]);
}

#[test]
fn unknown_subcommand_option_is_still_rejected() {
    let result = Cli::parse(["list", "--nope"].iter().map(|value| value.to_string()));
    assert!(result.is_err());
}

#[test]
fn parses_hold_disable() {
    let cli = Cli::parse(["hold", "--off", "Git.Git"].into_iter().map(str::to_string))
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Hold {
            packages: vec!["Git.Git".to_string()],
            enable: false,
            list: false
        }
    );
}

#[test]
fn parses_search_query() {
    let cli = Cli::parse(["search", "power", "toys"].into_iter().map(str::to_string))
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Search {
            query: "power toys".to_string(),
            deep: false
        }
    );
}

#[test]
fn parses_global_backend() {
    let cli = Cli::parse(
        ["--backend", "scoop", "install", "git"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(cli.backend, Some(Backend::Scoop));
    assert_eq!(
        cli.command,
        Subcommand::Install {
            mode: InstallMode::Search("git".to_string())
        }
    );
}

#[test]
fn parses_backend_enable_command() {
    let cli = Cli::parse(["backend", "enable", "pip"].into_iter().map(str::to_string))
        .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Backend {
            action: BackendAction::Enable {
                backend: Backend::Pip,
            }
        }
    );
}

#[test]
fn parses_backend_install_with_enable_flag() {
    let cli = Cli::parse(
        ["backend", "install", "npm", "--enable"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Backend {
            action: BackendAction::Install {
                backend: Backend::Npm,
                enable: true,
            }
        }
    );
}

#[test]
fn parses_backend_default_auto() {
    let cli = Cli::parse(
        ["--json", "backend", "default", "auto"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert!(cli.json);
    assert_eq!(
        cli.command,
        Subcommand::Backend {
            action: BackendAction::Default { backend: None }
        }
    );
}

#[test]
fn parses_json_show_command() {
    let cli = Cli::parse(
        ["--json", "show", "requests"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert!(cli.json);
    assert_eq!(
        cli.command,
        Subcommand::Show {
            package: "requests".to_string()
        }
    );
}

#[test]
fn parses_interactive_and_no_elevate_globals() {
    let cli = Cli::parse(
        ["--interactive", "--no-elevate", "upgrade"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(cli.assume_yes, Some(false));
    assert_eq!(cli.auto_elevate, Some(false));
    assert_eq!(cli.command, Subcommand::Upgrade { packages: vec![] });
}

#[test]
fn json_support_covers_show_list_and_search() {
    assert!(command_supports_json(&Subcommand::Show {
        package: "requests".to_string(),
    }));
    assert!(command_supports_json(&Subcommand::List {
        upgradable: false
    }));
    assert!(command_supports_json(&Subcommand::Search {
        query: "git".to_string(),
        deep: false
    }));
    assert!(!command_supports_json(&Subcommand::Backends));
    assert!(!command_supports_json(&Subcommand::Help));
}

#[test]
fn parses_install_search_by_default() {
    let cli = Cli::parse(
        ["install", "visual", "studio"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Install {
            mode: InstallMode::Search("visual studio".to_string())
        }
    );
}

#[test]
fn parses_install_pick_alias() {
    let cli = Cli::parse(
        ["install", "--pick", "visual", "studio"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Install {
            mode: InstallMode::Search("visual studio".to_string())
        }
    );
}

#[test]
fn parses_install_exact_mode() {
    let cli = Cli::parse(
        ["install", "--exact", "Git.Git", "Microsoft.PowerToys"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Install {
            mode: InstallMode::Exact(vec![
                PackageSpec::without_version("Git.Git"),
                PackageSpec::without_version("Microsoft.PowerToys")
            ])
        }
    );
}

#[test]
fn parses_remove_search_by_default() {
    let cli = Cli::parse(
        ["remove", "visual", "studio"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Remove {
            mode: RemoveMode::Search("visual studio".to_string())
        }
    );
}

#[test]
fn parses_remove_exact_mode() {
    let cli = Cli::parse(
        ["remove", "--exact", "Git.Git", "Microsoft.PowerToys"]
            .into_iter()
            .map(str::to_string),
    )
    .expect("cli should parse");

    assert_eq!(
        cli.command,
        Subcommand::Remove {
            mode: RemoveMode::Exact(vec![
                "Git.Git".to_string(),
                "Microsoft.PowerToys".to_string()
            ])
        }
    );
}

#[test]
fn parses_config_file() {
    let config = Config::parse(
        r#"
backend = "winget"
assume_yes = true
winget_source = "winget"
choco_source = "https://community.chocolatey.org/api/v2/"
scoop_bucket = "extras"
pip_user = true
"#,
    )
    .expect("config should parse");

    assert_eq!(config.backend, Some(Backend::Winget));
    assert!(config.assume_yes);
    assert_eq!(config.winget_source(), Some("winget"));
    assert_eq!(
        config.choco_source(),
        Some("https://community.chocolatey.org/api/v2/")
    );
    assert_eq!(config.qualify_scoop_package("git"), "extras/git");
    assert!(config.pip_user);
    assert!(config.auto_elevate);
    assert!(config.enable_winget);
    assert!(config.enable_pip);
}

#[test]
fn config_defaults_to_non_interactive_and_auto_elevate() {
    let config = Config::default();

    assert!(config.assume_yes);
    assert!(config.auto_elevate);
}

#[test]
fn process_elevation_is_skipped_for_upgrade_preview_before_selection() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let requires = command_requires_process_elevation(
        &Subcommand::Upgrade { packages: vec![] },
        Some(Backend::Winget),
        &runtime,
    )
    .expect("elevation check should succeed");

    assert!(!requires);
}

#[test]
fn process_elevation_is_skipped_for_install_search_before_selection() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let requires = command_requires_process_elevation(
        &Subcommand::Install {
            mode: InstallMode::Search("git".to_string()),
        },
        None,
        &runtime,
    )
    .expect("elevation check should succeed");

    assert!(!requires);
}

#[test]
fn process_elevation_is_skipped_for_user_pip_installs() {
    let config = Config {
        pip_user: true,
        ..Config::default()
    };
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let requires = command_requires_process_elevation(
        &Subcommand::Install {
            mode: InstallMode::Exact(vec![PackageSpec::without_version("requests")]),
        },
        Some(Backend::Pip),
        &runtime,
    )
    .expect("elevation check should succeed");

    assert!(!requires);
}

#[test]
fn backend_bootstrap_install_reports_when_automatic_execution_exists() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let requires = command_requires_process_elevation(
        &Subcommand::Backend {
            action: BackendAction::Install {
                backend: Backend::Chocolatey,
                enable: false,
            },
        },
        None,
        &runtime,
    )
    .expect("elevation check should succeed");

    assert!(!requires);
}

#[test]
fn npm_and_pip_bootstrap_only_exist_on_windows_hosts() {
    assert_eq!(
        Backend::Npm.install_invocation(true).is_some(),
        cfg!(windows)
    );
    assert_eq!(
        Backend::Pip.install_invocation(true).is_some(),
        cfg!(windows)
    );
}

#[test]
fn scoop_bootstrap_is_manual_only() {
    assert!(Backend::Scoop.install_invocation(true).is_none());
    assert!(Backend::Scoop.install_hint().contains("https://scoop.sh/"));
}

#[test]
fn elevated_child_command_redirects_stdout_and_stderr() {
    let command = build_elevated_child_command(
        OsStr::new(r"C:\Tools\waw.exe"),
        &[OsString::from("upgrade"), OsString::from("--all")],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
    );

    assert!(command.contains("Start-Process -FilePath 'C:\\Tools\\waw.exe'"));
    assert!(command.contains("@('upgrade', '--all')"));
    assert!(command.contains("-RedirectStandardOutput 'C:\\Temp\\waw.stdout.log'"));
    assert!(command.contains("-RedirectStandardError 'C:\\Temp\\waw.stderr.log'"));
    assert!(command.contains("chcp.com 65001 > $null"));
    assert!(command.contains("exit $code"));
}

#[test]
fn elevated_child_command_tolerates_winget_uninstall_not_found() {
    let command = build_elevated_child_command(
        OsStr::new(r"C:\Users\fallingstar\AppData\Local\Microsoft\WindowsApps\winget.exe"),
        &[
            OsString::from("uninstall"),
            OsString::from("--id"),
            OsString::from("ARP\\User\\X64\\ProxyPilot"),
            OsString::from("--exact"),
        ],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
    );

    assert!(command.contains("[uint32]$code -eq 0x800401F5"));
}

#[test]
fn elevated_child_command_tolerates_winget_no_applicable_upgrade() {
    let command = build_elevated_child_command(
        OsStr::new(r"C:\Users\fallingstar\AppData\Local\Microsoft\WindowsApps\winget.exe"),
        &[
            OsString::from("upgrade"),
            OsString::from("--id"),
            OsString::from("Posit.Quarto"),
            OsString::from("--exact"),
        ],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
    );

    assert!(command.contains("No applicable upgrade found"));
    assert!(command.contains("找不到适用的升级"));
}

#[test]
fn elevated_loader_command_reads_command_file() {
    let command = build_elevated_loader_command(
        OsStr::new(r"C:\Temp\elevated-command.txt"),
        OsStr::new(r"C:\Temp\waw.status.log"),
    );

    assert!(command.contains("$ErrorActionPreference = 'Stop'"));
    assert!(
        command.contains(
            "ReadAllText('C:\\Temp\\elevated-command.txt', [System.Text.Encoding]::UTF8)"
        )
    );
    assert!(command.contains("[scriptblock]::Create("));
    assert!(command.contains(
            "Add-Content -LiteralPath 'C:\\Temp\\waw.status.log' -Value ('WAW_ELEVATED_BOOTSTRAP_FAILURE:' + $wawMessage) -Encoding utf8"
        ));
}

#[test]
fn write_elevated_command_file_writes_utf8_bom() {
    let path = env::temp_dir().join(format!(
        "waw-elevated-command-test-{}-{}.txt",
        std::process::id(),
        APP_VERSION
    ));

    write_elevated_command_file(&path, "Write-Host 'hello'")
        .expect("command file should be written");
    let bytes = fs::read(&path).expect("script should be readable");

    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(String::from_utf8_lossy(&bytes[3..]).contains("Write-Host 'hello'"));

    fs::remove_file(&path).expect("script should be removed");
}

#[test]
fn elevated_batch_command_runs_each_invocation_and_redirects_logs() {
    let command = build_elevated_batch_command(
        &[
            Invocation::owned("winget", vec!["upgrade".to_string(), "--all".to_string()]),
            Invocation::owned(
                "npm.cmd",
                vec!["update".to_string(), "--global".to_string()],
            ),
        ],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
        OsStr::new(r"C:\Temp\waw.status.log"),
        false,
    );

    assert!(command.contains("& {"));
    assert!(command.contains("Start-Process -FilePath 'winget'"));
    assert!(command.contains("Start-Process -FilePath 'npm.cmd'"));
    assert!(command.contains("-ArgumentList @('upgrade', '--all')"));
    assert!(command.contains("-ArgumentList @('update', '--global')"));
    assert!(command.contains("-RedirectStandardOutput $wawStdout"));
    assert!(command.contains("-RedirectStandardError $wawStderr"));
    assert!(!command.contains("-WindowStyle Hidden"));
    assert!(command.contains("chcp.com 65001 > $null"));
    assert!(
        command
            .contains("[Console]::Error.WriteLine('WAW_ELEVATED_STEP:Upgrading winget packages')")
    );
    assert!(command.contains(
            "Add-Content -LiteralPath 'C:\\Temp\\waw.status.log' -Value 'WAW_ELEVATED_STEP:Upgrading winget packages' -Encoding utf8"
        ));
    assert!(
        command.contains(
            "[Console]::Error.WriteLine('WAW_ELEVATED_SUCCESS:Upgrading winget packages')"
        )
    );
    assert!(command.contains(
        "[Console]::Error.WriteLine('WAW_ELEVATED_FAILURE:Upgrading winget packages:' + $code)"
    ));
    assert!(command.contains(
            "Add-Content -LiteralPath 'C:\\Temp\\waw.status.log' -Value 'WAW_ELEVATED_SUCCESS:Upgrading winget packages' -Encoding utf8"
        ));
    assert!(command.contains(
            "Add-Content -LiteralPath 'C:\\Temp\\waw.status.log' -Value ('WAW_ELEVATED_FAILURE:Upgrading winget packages:' + $code) -Encoding utf8"
        ));
    // The child's output must be appended to the log files directly. `1>>`
    // cannot be used: the parent has those files open, and PowerShell's
    // redirection takes an exclusive handle. Relying on `[Console]::Out.Write`
    // alone lost the text, because console writes bypass stream redirection.
    assert!(!command.contains("1>> 'C:\\Temp\\waw.stdout.log'"));
    assert!(command.contains(
        "Add-Content -LiteralPath 'C:\\Temp\\waw.stdout.log' -Value $stdoutText -NoNewline -Encoding utf8"
    ));
    assert!(command.contains(
        "Add-Content -LiteralPath 'C:\\Temp\\waw.stderr.log' -Value $stderrText -NoNewline -Encoding utf8"
    ));
    // The console echo stays for a human watching the UAC window.
    assert!(command.contains("[Console]::Out.Write($stdoutText)"));
    assert!(command.contains("[Console]::Error.Write($stderrText)"));
}

#[test]
fn invocation_progress_label_includes_winget_target_package() {
    let invocation = Invocation::owned(
        "winget",
        vec![
            "upgrade".to_string(),
            "--id".to_string(),
            "Posit.Quarto".to_string(),
            "--exact".to_string(),
        ],
    );

    assert_eq!(
        invocation_progress_label(&invocation),
        "Upgrading winget: Posit.Quarto"
    );
}

#[test]
fn invocation_progress_label_says_removing_for_pip_uninstall() {
    // Regression: `-m pip uninstall` had no branch, so removal printed the
    // generic "Running pip...".
    let with_target = Invocation::owned(
        "python",
        vec![
            "-m".to_string(),
            "pip".to_string(),
            "uninstall".to_string(),
            "pyfiglet".to_string(),
            "--no-input".to_string(),
            "--yes".to_string(),
        ],
    );
    assert_eq!(
        invocation_progress_label(&with_target),
        "Removing pip: pyfiglet"
    );

    let without_target = Invocation::owned(
        "python",
        vec!["-m".to_string(), "pip".to_string(), "uninstall".to_string()],
    );
    assert_eq!(
        invocation_progress_label(&without_target),
        "Removing pip packages"
    );
}

#[test]
fn every_backend_uses_a_removal_label() {
    // Guard against another backend silently falling through to
    // "Running <backend>".
    let cases: [(&str, Vec<&str>); 4] = [
        ("winget", vec!["uninstall", "--id", "Git.Git", "--exact"]),
        ("scoop", vec!["uninstall", "git"]),
        ("choco", vec!["uninstall", "less", "-y"]),
        ("npm", vec!["uninstall", "--global", "cowsay"]),
    ];
    for (program, args) in cases {
        let invocation = Invocation::owned(program, args.into_iter().map(str::to_string).collect());
        let label = invocation_progress_label(&invocation);
        assert!(
            label.starts_with("Removing"),
            "{program} removal label was {label:?}"
        );
    }
}

#[test]
fn invocation_progress_label_includes_pip_target_package() {
    let invocation = Invocation::owned(
        "python",
        vec![
            "-m".to_string(),
            "pip".to_string(),
            "install".to_string(),
            "requests".to_string(),
        ],
    );

    assert_eq!(
        invocation_progress_label(&invocation),
        "Installing pip: requests"
    );
}

#[test]
fn elevated_batch_command_tolerates_winget_uninstall_not_found() {
    let command = build_elevated_batch_command(
        &[Invocation::owned(
            "winget",
            vec![
                "uninstall".to_string(),
                "--id".to_string(),
                "ARP\\User\\X64\\ProxyPilot".to_string(),
                "--exact".to_string(),
            ],
        )],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
        OsStr::new(r"C:\Temp\waw.status.log"),
        false,
    );

    assert!(command.contains("[uint32]$code -eq 0x800401F5"));
}

#[test]
fn elevated_batch_command_tolerates_winget_no_applicable_upgrade() {
    let command = build_elevated_batch_command(
        &[Invocation::owned(
            "winget",
            vec![
                "upgrade".to_string(),
                "--id".to_string(),
                "Posit.Quarto".to_string(),
                "--exact".to_string(),
            ],
        )],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
        OsStr::new(r"C:\Temp\waw.status.log"),
        false,
    );

    assert!(command.contains("No applicable upgrade found"));
    assert!(command.contains("找不到适用的升级"));
}

#[test]
fn elevated_batch_command_can_continue_after_failure() {
    let command = build_elevated_batch_command(
        &[Invocation::owned(
            "npm.cmd",
            vec!["update".to_string(), "code-server@latest".to_string()],
        )],
        OsStr::new(r"C:\Temp\waw.stdout.log"),
        OsStr::new(r"C:\Temp\waw.stderr.log"),
        OsStr::new(r"C:\Temp\waw.status.log"),
        true,
    );

    assert!(command.contains("$script:wawBatchExitCode = 0;"));
    assert!(
        command
            .contains("if ($script:wawBatchExitCode -eq 0) { $script:wawBatchExitCode = $code }")
    );
    assert!(command.contains(
        "if ($script:wawBatchExitCode -ne 0) { exit $script:wawBatchExitCode } else { exit 0 }"
    ));
}

#[test]
fn decode_capture_text_handles_utf16le_with_bom() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(utf16le_bytes("winget upgrade\r\n"));

    assert_eq!(decode_capture_text(&bytes, false), "winget upgrade\r\n");
    assert_eq!(decode_capture_text(&bytes, true), "winget upgrade\r\n");
}

#[test]
fn decode_capture_text_handles_utf16le_without_bom() {
    let bytes = utf16le_bytes("source update\r\n");

    assert_eq!(decode_capture_text(&bytes, false), "source update\r\n");
    assert_eq!(decode_capture_text(&bytes, true), "source update\r\n");
}

#[test]
fn decode_capture_text_defers_incomplete_utf8_until_final_chunk() {
    let bytes = vec![b'o', b'k', b' ', 0xE4, 0xB8];

    assert_eq!(decode_capture_text(&bytes, false), "ok ");
    assert_eq!(decode_capture_text(&bytes, true), "ok �");
}

#[test]
fn parse_elevated_failure_extracts_latest_failure_marker() {
    let stderr_log = concat!(
        "warning output\r\n",
        "WAW_ELEVATED_STEP:Upgrading winget packages\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading winget packages:5\r\n",
        "WAW_ELEVATED_STEP:Upgrading npm global packages\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading npm global packages:17\r\n"
    );

    assert_eq!(
        parse_elevated_failure(stderr_log),
        Some(("Upgrading npm global packages".to_string(), 17))
    );
}

#[test]
fn parse_elevated_failure_handles_utf16le_capture_logs() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(utf16le_bytes(
        "WAW_ELEVATED_FAILURE:Upgrading winget packages:7\r\n",
    ));
    let stderr_log = decode_capture_text(&bytes, true);

    assert_eq!(
        parse_elevated_failure(&stderr_log),
        Some(("Upgrading winget packages".to_string(), 7))
    );
}

#[test]
fn parse_elevated_failure_accepts_unsigned_windows_exit_code() {
    assert_eq!(
        parse_elevated_failure("WAW_ELEVATED_FAILURE:Removing winget: ProxyPilot:2147746293\r\n"),
        Some(("Removing winget: ProxyPilot".to_string(), -2147221003))
    );
}

#[test]
fn winget_no_applicable_upgrade_message_matches_english_and_chinese() {
    assert!(winget_no_applicable_upgrade_message(
        "No applicable upgrade found; a newer package version is available in the configured sources, but it does not apply to your system or requirements."
    ));
    assert!(winget_no_applicable_upgrade_message(
        "找不到适用的升级，较新的包在配置的源中可用，但不适用于你的系统或要求"
    ));
    assert!(!winget_no_applicable_upgrade_message("generic failure"));
}

#[test]
fn tolerable_command_failure_recognizes_winget_upgrade_skip() {
    let invocation = Invocation::owned(
        "winget",
        vec![
            "upgrade".to_string(),
            "--id".to_string(),
            "Posit.Quarto".to_string(),
            "--exact".to_string(),
        ],
    );
    let capture = CommandCapture {
        stdout: String::new(),
        stderr: "找不到适用的升级，较新的包在配置的源中可用，但不适用于你的系统或要求".to_string(),
        success: false,
        status_code: 1,
        timed_out: false,
        timeout_seconds: None,
    };

    assert!(is_tolerable_command_failure(&invocation, &capture));
}

#[test]
fn tolerable_command_failure_recognizes_winget_upgrade_exit_code() {
    let invocation = Invocation::owned(
        "winget",
        vec![
            "upgrade".to_string(),
            "--id".to_string(),
            "Microsoft.Edge".to_string(),
            "--exact".to_string(),
        ],
    );
    let capture = CommandCapture {
        stdout: String::new(),
        stderr: String::new(),
        success: false,
        status_code: WINGET_NO_APPLICABLE_UPGRADE_EXIT_CODE,
        timed_out: false,
        timeout_seconds: None,
    };

    assert!(is_tolerable_command_failure(&invocation, &capture));
}

#[test]
fn winget_upgrade_elevated_snippet_tolerates_known_exit_code() {
    let snippet = tolerated_elevated_exit_snippet(
        "winget",
        &[
            "upgrade".to_string(),
            "--id".to_string(),
            "Microsoft.Edge".to_string(),
        ],
    );

    assert!(snippet.contains("[int]$code -eq -1978335189"));
    assert!(snippet.contains("[uint32]$code -eq 0x8A15002B"));
}

#[test]
fn visible_elevated_stderr_strips_internal_markers() {
    let stderr_log = concat!(
        "WAW_ELEVATED_STEP:Upgrading winget packages\r\n",
        "WAW_ELEVATED_SUCCESS:Upgrading winget packages\r\n",
        "WAW_ELEVATED_BOOTSTRAP_FAILURE:loader failed\r\n",
        "real stderr line\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading winget packages:9\r\n"
    );

    assert_eq!(visible_elevated_stderr(stderr_log), "real stderr line\n");
}

#[test]
fn summarize_elevated_batch_distinguishes_success_failure_and_not_started() {
    let invocations = vec![
        Invocation::owned(
            "npm.cmd",
            vec!["update".to_string(), "code-server@latest".to_string()],
        ),
        Invocation::owned(
            "npm.cmd",
            vec!["update".to_string(), "opencode-ai@latest".to_string()],
        ),
        Invocation::owned(
            "python",
            vec![
                "-m".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
                "pydantic".to_string(),
            ],
        ),
    ];
    let stderr_log = concat!(
        "WAW_ELEVATED_STEP:Upgrading npm: code-server@latest\r\n",
        "WAW_ELEVATED_SUCCESS:Upgrading npm: code-server@latest\r\n",
        "WAW_ELEVATED_STEP:Upgrading npm: opencode-ai@latest\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading npm: opencode-ai@latest:1\r\n",
    );

    assert_eq!(
        summarize_elevated_batch(&invocations, stderr_log),
        vec![
            ElevatedBatchStepSummary {
                label: "Upgrading npm: code-server@latest".to_string(),
                status: ElevatedBatchStepStatus::Succeeded,
            },
            ElevatedBatchStepSummary {
                label: "Upgrading npm: opencode-ai@latest".to_string(),
                status: ElevatedBatchStepStatus::Failed(1),
            },
            ElevatedBatchStepSummary {
                label: "Upgrading pip: pydantic".to_string(),
                status: ElevatedBatchStepStatus::NotStarted,
            },
        ]
    );
}

#[test]
fn render_elevated_batch_summary_lists_succeeded_failed_and_not_started() {
    let invocations = vec![
        Invocation::owned(
            "npm.cmd",
            vec!["update".to_string(), "code-server@latest".to_string()],
        ),
        Invocation::owned(
            "npm.cmd",
            vec!["update".to_string(), "opencode-ai@latest".to_string()],
        ),
        Invocation::owned(
            "python",
            vec![
                "-m".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
                "pydantic".to_string(),
            ],
        ),
    ];
    let stderr_log = concat!(
        "WAW_ELEVATED_STEP:Upgrading npm: code-server@latest\r\n",
        "WAW_ELEVATED_SUCCESS:Upgrading npm: code-server@latest\r\n",
        "WAW_ELEVATED_STEP:Upgrading npm: opencode-ai@latest\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading npm: opencode-ai@latest:1\r\n",
    );

    assert_eq!(
        render_elevated_batch_summary(&invocations, stderr_log),
        Some(
            [
                "Batch summary:",
                "Succeeded (1):",
                "- Upgrading npm: code-server@latest",
                "Failed (1):",
                "- Upgrading npm: opencode-ai@latest (exit code 1)",
                "Not started (1):",
                "- Upgrading pip: pydantic",
            ]
            .join("\n")
        )
    );
}

#[test]
fn render_invocation_batch_summary_lists_succeeded_and_failed() {
    let succeeded = vec![
        "Upgrading winget: KaringX.Karing".to_string(),
        "Upgrading pip: pydantic".to_string(),
    ];
    let failed = vec![(
        "Installing npm: code-server@latest".to_string(),
        "backend command failed with exit code 1: npm update --global code-server@latest"
            .to_string(),
    )];

    assert_eq!(
            render_invocation_batch_summary(&succeeded, &failed),
            Some(
                [
                    "Batch summary:",
                    "Succeeded (2):",
                    "- Upgrading winget: KaringX.Karing",
                    "- Upgrading pip: pydantic",
                    "Failed (1):",
                    "- Installing npm: code-server@latest: backend command failed with exit code 1: npm update --global code-server@latest",
                ]
                .join("\n")
            )
        );
}

#[test]
fn format_elevated_batch_failure_prefers_precise_failure_marker() {
    let stderr_log = "WAW_ELEVATED_FAILURE:Upgrading winget packages:9\r\n";

    assert_eq!(
        format_elevated_batch_failure(1, stderr_log),
        "backend command failed with exit code 9: Upgrading winget packages"
    );
}

#[test]
fn format_elevated_batch_failure_falls_back_without_marker() {
    assert_eq!(
        format_elevated_batch_failure(13, "plain stderr output"),
        "one or more elevated backend commands failed with exit code 13"
    );
}

#[test]
fn format_elevated_batch_failure_reports_bootstrap_reason() {
    assert_eq!(
        format_elevated_batch_failure(
            1,
            "WAW_ELEVATED_BOOTSTRAP_FAILURE:The system cannot find the file specified.\r\n"
        ),
        "elevated bootstrap failed: The system cannot find the file specified."
    );
}

#[test]
fn format_elevated_batch_failure_reports_multiple_failures() {
    let stderr_log = concat!(
        "WAW_ELEVATED_FAILURE:Installing npm: code-server@latest:1\r\n",
        "WAW_ELEVATED_FAILURE:Upgrading pip: pydantic:1\r\n",
    );

    assert_eq!(
        format_elevated_batch_failure(1, stderr_log),
        "2 backend commands failed; see batch summary above"
    );
}

#[test]
fn winget_upgrade_all_plan_matches_expectation() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Winget
        .plan(&Subcommand::Upgrade { packages: vec![] }, &runtime)
        .expect("plan should build");

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["winget", "winget.exe"]);
    // winget installs per-user, so it must not request elevation. Demanding
    // admin here meant every `waw install` raised a UAC prompt.
    assert!(
        !plan[0].requires_elevation,
        "winget must not require elevation"
    );
    assert_eq!(
        plan[0].args,
        vec![
            "upgrade".to_string(),
            "--all".to_string(),
            "--include-unknown".to_string(),
            "--accept-source-agreements".to_string(),
            "--accept-package-agreements".to_string(),
        ]
    );
}

#[test]
fn winget_upgrade_all_with_yes_is_non_interactive() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Winget
        .plan(&Subcommand::Upgrade { packages: vec![] }, &runtime)
        .expect("plan should build");

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["winget", "winget.exe"]);
    assert!(plan[0].args.contains(&"--silent".to_string()));
    assert!(
        plan[0]
            .args
            .contains(&"--disable-interactivity".to_string())
    );
}

#[test]
fn winget_remove_target_with_version_uses_version_flag() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Winget.plan_remove(
        &[RemoveTarget {
            package: "Posit.Quarto".to_string(),
            version: Some("1.8.26".to_string()),
        }],
        &runtime,
    );

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["winget", "winget.exe"]);
    assert_eq!(
        plan[0].args,
        vec![
            "uninstall".to_string(),
            "--id".to_string(),
            "Posit.Quarto".to_string(),
            "--exact".to_string(),
            "--version".to_string(),
            "1.8.26".to_string(),
            "--silent".to_string(),
            "--disable-interactivity".to_string(),
        ]
    );
}

#[test]
fn winget_search_uses_configured_source() {
    let config = Config {
        winget_source: Some("winget".to_string()),
        ..Config::default()
    };
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Winget
        .plan(
            &Subcommand::Search {
                query: "git".to_string(),
                deep: false,
            },
            &runtime,
        )
        .expect("plan should build");

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["winget", "winget.exe"]);
    assert_eq!(
        plan[0].args,
        vec![
            "search".to_string(),
            "git".to_string(),
            "--source".to_string(),
            "winget".to_string(),
        ]
    );
}

#[test]
fn scoop_install_uses_bucket_prefix() {
    let config = Config {
        scoop_buckets: vec!["extras".to_string()],
        scoop_default_bucket: Some("extras".to_string()),
        ..Config::default()
    };
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Scoop
        .plan(
            &Subcommand::Install {
                mode: InstallMode::Exact(vec![PackageSpec::without_version("git")]),
            },
            &runtime,
        )
        .expect("plan should build");

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["scoop", "scoop.cmd", "scoop.ps1"]);
    assert_eq!(
        plan[0].args,
        vec!["install".to_string(), "extras/git".to_string()]
    );
}

#[test]
fn scoop_bucket_qualification_matrix() {
    let no_bucket = Config::default();
    assert_eq!(no_bucket.qualify_scoop_package("git"), "git");
    assert_eq!(no_bucket.qualify_scoop_package("gh@2.7.0"), "gh@2.7.0");

    // `main` is Scoop's implicit default; prefixing it changes nothing but
    // would break version specs.
    let main = Config {
        scoop_default_bucket: Some("main".to_string()),
        ..Config::default()
    };
    assert_eq!(main.qualify_scoop_package("git"), "git");

    let extras = Config {
        scoop_default_bucket: Some("extras".to_string()),
        ..Config::default()
    };
    assert_eq!(extras.qualify_scoop_package("gh"), "extras/gh");
    // Version suffix must survive the prefix.
    assert_eq!(extras.qualify_scoop_package("gh@2.7.0"), "extras/gh@2.7.0");
    // Already-qualified names are untouched.
    assert_eq!(extras.qualify_scoop_package("main/git"), "main/git");
    assert_eq!(
        extras.qualify_scoop_package("extras/gh@2.7.0"),
        "extras/gh@2.7.0"
    );
}

#[test]
fn legacy_scoop_bucket_key_maps_to_default() {
    let config = Config::parse("scoop_bucket = \"extras\"\n").expect("should parse");
    assert_eq!(config.scoop_default_bucket.as_deref(), Some("extras"));
    assert_eq!(config.scoop_buckets, vec!["extras".to_string()]);
    // The behaviour users relied on still happens.
    assert_eq!(config.qualify_scoop_package("gh"), "extras/gh");
}

#[test]
fn scoop_buckets_parse_and_round_trip() {
    let config = Config::parse(
        "scoop_buckets = [\"main\", \"extras\", \"versions\"]\nscoop_default_bucket = \"extras\"\n",
    )
    .expect("should parse");
    assert_eq!(
        config.scoop_buckets,
        vec![
            "main".to_string(),
            "extras".to_string(),
            "versions".to_string()
        ]
    );
    assert_eq!(config.scoop_default_bucket.as_deref(), Some("extras"));

    let rendered = config.to_toml();
    let reparsed = Config::parse(&rendered).expect("round trip should parse");
    assert_eq!(reparsed.scoop_buckets, config.scoop_buckets);
    assert_eq!(reparsed.scoop_default_bucket, config.scoop_default_bucket);
}

#[test]
fn scoop_buckets_accepts_a_bare_scalar() {
    let config = Config::parse("scoop_buckets = \"extras\"\n").expect("should parse");
    assert_eq!(config.scoop_buckets, vec!["extras".to_string()]);
}

#[test]
fn empty_arguments_are_rejected_instead_of_reaching_the_backend() {
    // `search ""` used to pass an empty query through, which made scoop
    // return every single-letter package; `show ""` made scoop print its
    // own usage text and waw present that as package details.
    for args in [
        vec!["search", ""],
        vec!["search", "   "],
        vec!["show", ""],
        vec!["install", "--exact", ""],
        vec!["install", "--exact", "  "],
    ] {
        let result = Cli::parse(args.iter().map(|value| value.to_string()));
        assert!(
            result.is_err(),
            "{args:?} should be rejected, got {result:?}"
        );
    }
}

#[test]
fn non_empty_arguments_still_parse() {
    assert!(Cli::parse(["search", "git"].iter().map(|v| v.to_string())).is_ok());
    assert!(Cli::parse(["show", "git"].iter().map(|v| v.to_string())).is_ok());
    assert!(Cli::parse(["install", "--exact", "git"].iter().map(|v| v.to_string())).is_ok());
}

#[test]
fn unterminated_array_is_rejected() {
    assert!(parse_string_list("[\"main\", \"extras\"").is_err());
    assert!(parse_string_list("]").is_err());
    assert!(parse_string_list("[\"main\"]").is_ok());
    assert!(parse_string_list("\"main\"").is_ok());
    assert!(parse_string_list("").is_ok());
}

#[test]
fn parse_string_list_handles_arrays_and_scalars() {
    assert_eq!(
        parse_string_list("\"a\"").expect("ok"),
        vec!["a".to_string()]
    );
    assert_eq!(parse_string_list("[]").expect("ok"), Vec::<String>::new());
    assert_eq!(
        parse_string_list("[\"a\", \"b\"]").expect("ok"),
        vec!["a".to_string(), "b".to_string()]
    );
    // Duplicates collapse.
    assert_eq!(
        parse_string_list("[\"a\",\"a\"]").expect("ok"),
        vec!["a".to_string()]
    );
}

#[test]
fn chocolatey_hold_plan_uses_pin_add() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };
    let plan = Backend::Chocolatey.plan_hold_pin(&["git".to_string()], true, &runtime);

    assert_eq!(plan.len(), 1);
    assert_program_matches(&plan[0].program, &["choco", "choco.exe", "choco.bat"]);
    assert_eq!(
        plan[0].args,
        vec![
            "pin".to_string(),
            "add".to_string(),
            "--name".to_string(),
            "git".to_string(),
        ]
    );
}

#[test]
fn holding_through_backend_plan_is_a_no_op() {
    // `hold` is routed to commands/hold.rs: it may need to resolve an id
    // or write holds.toml, neither of which is one invocation.
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let plan = Backend::Winget
        .plan(
            &Subcommand::Hold {
                packages: vec!["Git.Git".to_string()],
                enable: true,
                list: false,
            },
            &runtime,
        )
        .expect("plan should build");

    assert!(plan.is_empty());
}

#[test]
fn parses_hold_list_flag() {
    let cli =
        Cli::parse(["hold", "--list"].into_iter().map(str::to_string)).expect("cli should parse");
    assert_eq!(
        cli.command,
        Subcommand::Hold {
            packages: Vec::new(),
            enable: true,
            list: true
        }
    );

    let cli = Cli::parse(["hold", "-l"].into_iter().map(str::to_string)).expect("cli should parse");
    assert!(matches!(cli.command, Subcommand::Hold { list: true, .. }));
}

#[test]
fn hold_list_rejects_package_names() {
    let result = Cli::parse(["hold", "--list", "git"].into_iter().map(str::to_string));
    assert!(result.is_err());
    let message = result.unwrap_err();
    assert!(message.contains("--list"), "{message}");
}

#[test]
fn hold_without_packages_or_list_is_an_error() {
    let result = Cli::parse(["hold"].into_iter().map(str::to_string));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("--list"));
}

#[test]
fn winget_hold_list_plan_uses_pin_list() {
    let invocation = Backend::Winget
        .plan_hold_list()
        .expect("winget can enumerate pins");
    assert_eq!(invocation.args, vec!["pin".to_string(), "list".to_string()]);
    assert_program_matches(&invocation.program, &["winget", "winget.exe"]);
}

#[test]
fn scoop_hold_list_uses_scoop_list_because_hold_has_no_listing() {
    let invocation = Backend::Scoop
        .plan_hold_list()
        .expect("scoop exposes hold state through list");
    assert_eq!(invocation.args, vec!["list".to_string()]);
}

#[test]
fn backends_without_pin_enumeration_return_none() {
    // Their holds live in waw's own file, so there is nothing to ask.
    assert!(Backend::Npm.plan_hold_list().is_none());
    assert!(Backend::Pip.plan_hold_list().is_none());
}

#[test]
fn parses_winget_localized_pin_table() {
    // Localized headers, so the parser keys off the dashed rule instead.
    let output = concat!(
        "名称 ID 版本 源 包钉类型\r\n",
        "-------------------------------------\r\n",
        "Git Git.Git 2.52.0 winget Blocking\r\n",
        "PowerToys Microsoft.PowerToys 0.95.0 winget Pinned\r\n",
    );

    assert_eq!(
        Backend::Winget.parse_holds(output),
        vec![
            ("Git.Git".to_string(), "Blocking".to_string()),
            ("Microsoft.PowerToys".to_string(), "Pinned".to_string()),
        ]
    );
}

#[test]
fn parses_chocolatey_pin_list() {
    let output = concat!("Chocolatey v2.4.1\r\n", "git\r\n", "vlc\r\n");

    assert_eq!(
        Backend::Chocolatey.parse_holds(output),
        vec![
            ("git".to_string(), "pinned".to_string()),
            ("vlc".to_string(), "pinned".to_string()),
        ]
    );
}

#[test]
fn chocolatey_pin_list_ignores_prose_lines() {
    // `choco pin list` with nothing pinned prints a banner and a sentence,
    // neither of which is a package name.
    let output = concat!(
        "Chocolatey v2.4.1\r\n",
        "No pins were found in your configuration.\r\n",
    );

    assert!(Backend::Chocolatey.parse_holds(output).is_empty());
}

#[test]
fn winget_pin_parser_ignores_text_before_the_rule() {
    let output = concat!(
        "No pins matched the given criteria.\r\n",
        "\r\n",
        "名称 ID 版本 源 包钉类型\r\n",
        "-------------------\r\n",
    );

    assert!(Backend::Winget.parse_holds(output).is_empty());
}

#[test]
fn parses_selection_ranges_and_deduplicates() {
    let selected = parse_selection("1 3-4 4,2", 5).expect("selection should parse");
    assert_eq!(selected, vec![1, 3, 4, 2]);
}

#[test]
fn package_list_query_matches_across_multiple_fields() {
    let entry = PackageListEntry {
        backend: Backend::Scoop,
        name: "QQ NT".to_string(),
        package_id: "qq-nt".to_string(),
        current_version: "9.9.29".to_string(),
        available_version: Some("9.9.30".to_string()),
    };

    assert!(package_list_entry_matches_query(&entry, "qq"));
    assert!(package_list_entry_matches_query(&entry, "qq nt"));
    assert!(package_list_entry_matches_query(&entry, "qqnt"));
    assert!(package_list_entry_matches_query(&entry, "scoop qq"));
    assert!(package_list_entry_matches_query(&entry, "9.9.29"));
    assert!(package_list_entry_matches_query(&entry, "9.9.30"));
    assert!(!package_list_entry_matches_query(&entry, "winget"));
    assert!(!package_list_entry_matches_query(&entry, "telegram"));
}

#[test]
fn compact_search_rank_matches_separator_insensitive_queries() {
    assert_eq!(search_match_rank("qq nt", "qq-nt", "qqnt"), 0);
    assert_eq!(search_match_rank("qq music", "qq-music", "qqmu"), 1);
    assert_eq!(
        search_match_rank("lite loader", "liteloader-qqnt", "qqnt"),
        2
    );
}

#[test]
fn parses_scoop_search_candidates_from_bucket_sections() {
    let candidates = Backend::Scoop.parse_search_candidates(
            "'main' bucket:\n7zip 24.09\nWARN ignored\n'versions' bucket:\npython310 3.10.11\nNo Matches Found\n",
        );

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Scoop,
                label: "7zip".to_string(),
                install_id: "main/7zip".to_string(),
                version: Some("24.09".to_string()),
                source: Some("main".to_string()),
            },
            SearchCandidate {
                backend: Backend::Scoop,
                label: "python310".to_string(),
                install_id: "versions/python310".to_string(),
                version: Some("3.10.11".to_string()),
                source: Some("versions".to_string()),
            }
        ]
    );
}

#[test]
fn parses_choco_search_candidates_with_limit_output() {
    let candidates = Backend::Chocolatey.parse_search_candidates("git|2.48.1\nripgrep|14.1.1\n");

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Chocolatey,
                label: "git".to_string(),
                install_id: "git".to_string(),
                version: Some("2.48.1".to_string()),
                source: None,
            },
            SearchCandidate {
                backend: Backend::Chocolatey,
                label: "ripgrep".to_string(),
                install_id: "ripgrep".to_string(),
                version: Some("14.1.1".to_string()),
                source: None,
            }
        ]
    );
}

#[test]
fn parses_winget_search_candidates_from_table_output() {
    let output = "\
Name                         Id                           Version Source\n\
-----------------------------------------------------------------------\n\
Git                          Git.Git                      2.47.1  winget\n\
Microsoft PowerToys          Microsoft.PowerToys          0.90.1  winget\n";

    let candidates = Backend::Winget.parse_search_candidates(output);

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Winget,
                label: "Git".to_string(),
                install_id: "Git.Git".to_string(),
                version: Some("2.47.1".to_string()),
                source: Some("winget".to_string()),
            },
            SearchCandidate {
                backend: Backend::Winget,
                label: "Microsoft PowerToys".to_string(),
                install_id: "Microsoft.PowerToys".to_string(),
                version: Some("0.90.1".to_string()),
                source: Some("winget".to_string()),
            }
        ]
    );
}

#[test]
fn parses_winget_search_candidates_from_single_spaced_rows() {
    let output = "\
Name Id Version Source\n\
----------------------\n\
Git Git.Git 2.45 winget\n\
";

    let candidates = Backend::Winget.parse_search_candidates(output);

    assert_eq!(
        candidates,
        vec![SearchCandidate {
            backend: Backend::Winget,
            label: "Git".to_string(),
            install_id: "Git.Git".to_string(),
            version: Some("2.45".to_string()),
            source: Some("winget".to_string()),
        }]
    );
}

#[test]
fn parses_winget_search_candidates_with_match_column() {
    let output = "\
Name                         Id                           Version Match        Source\n\
-----------------------------------------------------------------------------------\n\
Git                          Git.Git                      2.47.1  Moniker: git winget\n\
";

    let candidates = Backend::Winget.parse_search_candidates(output);

    assert_eq!(
        candidates,
        vec![SearchCandidate {
            backend: Backend::Winget,
            label: "Git".to_string(),
            install_id: "Git.Git".to_string(),
            version: Some("2.47.1".to_string()),
            source: Some("winget".to_string()),
        }]
    );
}

#[test]
fn parses_winget_installed_list_with_localized_headers() {
    let output = "\
名称                         标识符                         版本     源\n\
-----------------------------------------------------------------------\n\
Git                          Git.Git                        2.47.1   winget\n\
Microsoft PowerToys          Microsoft.PowerToys            0.90.1   winget\n\
";

    assert_eq!(
        Backend::Winget.parse_list_entries(false, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Winget,
                name: "Git".to_string(),
                package_id: "Git.Git".to_string(),
                current_version: "2.47.1".to_string(),
                available_version: None,
            },
            PackageListEntry {
                backend: Backend::Winget,
                name: "Microsoft PowerToys".to_string(),
                package_id: "Microsoft.PowerToys".to_string(),
                current_version: "0.90.1".to_string(),
                available_version: None,
            }
        ])
    );
}

#[test]
fn parses_winget_upgrade_list_with_localized_headers() {
    let output = "\
名称                         标识符                         版本     可用     源\n\
--------------------------------------------------------------------------------\n\
Git                          Git.Git                        2.47.1   2.48.0   winget\n\
";

    assert_eq!(
        Backend::Winget.parse_list_entries(true, output),
        Some(vec![PackageListEntry {
            backend: Backend::Winget,
            name: "Git".to_string(),
            package_id: "Git.Git".to_string(),
            current_version: "2.47.1".to_string(),
            available_version: Some("2.48.0".to_string()),
        }])
    );
}

#[test]
fn parses_npm_search_candidates_from_warning_prefixed_array() {
    let output = r#"npm warn config global
[
  { "name": "left-pad", "version": "1.3.0" },
  { "name": "@types/node", "version": "24.0.0" }
]"#;

    let candidates = Backend::Npm.parse_search_candidates(output);

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Npm,
                label: "left-pad".to_string(),
                install_id: "left-pad".to_string(),
                version: Some("1.3.0".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Npm,
                label: "@types/node".to_string(),
                install_id: "@types/node".to_string(),
                version: Some("24.0.0".to_string()),
                source: Some("npm".to_string()),
            }
        ]
    );
}

#[test]
fn parses_pip_search_candidates_from_python_script_output() {
    let candidates = Backend::Pip.parse_search_candidates("requests\nrequests-cache\n");

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Pip,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            },
            SearchCandidate {
                backend: Backend::Pip,
                label: "requests-cache".to_string(),
                install_id: "requests-cache".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            }
        ]
    );
}

#[test]
fn parses_npm_installed_list_from_json() {
    let output = r#"{
  "name": "lib",
  "dependencies": {
    "@openai/codex": {
      "version": "0.121.0",
      "overridden": false
    },
    "npm": {
      "version": "11.12.1",
      "overridden": false
    }
  }
}"#;

    assert_eq!(
        Backend::Npm.parse_list_entries(false, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Npm,
                name: "@openai/codex".to_string(),
                package_id: "@openai/codex".to_string(),
                current_version: "0.121.0".to_string(),
                available_version: None,
            },
            PackageListEntry {
                backend: Backend::Npm,
                name: "npm".to_string(),
                package_id: "npm".to_string(),
                current_version: "11.12.1".to_string(),
                available_version: None,
            }
        ])
    );
}

#[test]
fn parses_npm_upgradable_list_from_json() {
    let output = r#"{
  "@openai/codex": {
    "current": "0.118.0",
    "wanted": "0.121.0",
    "latest": "0.121.0"
  },
  "happy-coder": {
    "current": "0.13.0",
    "wanted": "0.13.1",
    "latest": "0.13.1"
  }
}"#;

    assert_eq!(
        Backend::Npm.parse_list_entries(true, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Npm,
                name: "@openai/codex".to_string(),
                package_id: "@openai/codex".to_string(),
                current_version: "0.118.0".to_string(),
                available_version: Some("0.121.0".to_string()),
            },
            PackageListEntry {
                backend: Backend::Npm,
                name: "happy-coder".to_string(),
                package_id: "happy-coder".to_string(),
                current_version: "0.13.0".to_string(),
                available_version: Some("0.13.1".to_string()),
            }
        ])
    );
}

#[test]
fn parses_pip_installed_list_from_json() {
    let output = r#"[
  {"name": "certifi", "version": "2026.2.25"},
  {"name": "pip", "version": "26.0"}
]"#;

    assert_eq!(
        Backend::Pip.parse_list_entries(false, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Pip,
                name: "certifi".to_string(),
                package_id: "certifi".to_string(),
                current_version: "2026.2.25".to_string(),
                available_version: None,
            },
            PackageListEntry {
                backend: Backend::Pip,
                name: "pip".to_string(),
                package_id: "pip".to_string(),
                current_version: "26.0".to_string(),
                available_version: None,
            }
        ])
    );
}

#[test]
fn parses_scoop_installed_list_with_source_column() {
    let output = "\
Installed apps:

Name    Version  Source  Updated             Info
----    -------  ------  -------             ----
git     2.49.0   main    2026-04-17 13:00:00
neovim  0.10.4   extras  2026-04-16 10:30:00
";

    assert_eq!(
        Backend::Scoop.parse_list_entries(false, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Scoop,
                name: "git".to_string(),
                package_id: "git".to_string(),
                current_version: "2.49.0".to_string(),
                available_version: None,
            },
            PackageListEntry {
                backend: Backend::Scoop,
                name: "neovim".to_string(),
                package_id: "neovim".to_string(),
                current_version: "0.10.4".to_string(),
                available_version: None,
            }
        ])
    );
}

#[test]
fn parses_scoop_installed_list_ignoring_failed_rows() {
    let output = "\
Installed apps:

Name         Version      Source Updated             Info
----         -------      ------ -------             ----
7zip         26.00        main   2026-04-16 10:54:56
flclash                          2025-03-19 21:36:37 Install failed
git          2.53.0.3     main   2026-04-16 12:13:01
telegram                         2025-02-18 15:42:10 Install failed
";

    assert_eq!(
        Backend::Scoop.parse_list_entries(false, output),
        Some(vec![
            PackageListEntry {
                backend: Backend::Scoop,
                name: "7zip".to_string(),
                package_id: "7zip".to_string(),
                current_version: "26.00".to_string(),
                available_version: None,
            },
            PackageListEntry {
                backend: Backend::Scoop,
                name: "git".to_string(),
                package_id: "git".to_string(),
                current_version: "2.53.0.3".to_string(),
                available_version: None,
            }
        ])
    );
}

#[test]
fn parses_pip_upgradable_list_from_json() {
    let output = r#"[
  {
    "name": "pip",
    "version": "26.0",
    "latest_version": "26.0.1",
    "latest_filetype": "wheel"
  }
]"#;

    assert_eq!(
        Backend::Pip.parse_list_entries(true, output),
        Some(vec![PackageListEntry {
            backend: Backend::Pip,
            name: "pip".to_string(),
            package_id: "pip".to_string(),
            current_version: "26.0".to_string(),
            available_version: Some("26.0.1".to_string()),
        }])
    );
}

#[test]
fn parses_pip_show_details_from_key_value_output() {
    let output = "\
Name: pip
Version: 26.0
Summary: The PyPA recommended tool for installing Python packages.
Home-page: https://pip.pypa.io/
Author-email: The pip developers <distutils-sig@python.org>
License-Expression: MIT
Location: /opt/homebrew/lib/python3.14/site-packages
Requires: setuptools, wheel
Required-by:
";

    assert_eq!(
        Backend::Pip.parse_show_details(output),
        Some(PackageDetails {
            backend: Backend::Pip,
            name: "pip".to_string(),
            version: "26.0".to_string(),
            summary: Some("The PyPA recommended tool for installing Python packages.".to_string()),
            homepage: Some("https://pip.pypa.io/".to_string()),
            license: Some("MIT".to_string()),
            author: Some("The pip developers <distutils-sig@python.org>".to_string()),
            repository: None,
            keywords: vec![],
            dependencies: vec!["setuptools".to_string(), "wheel".to_string()],
            extra_fields: vec![(
                "Location".to_string(),
                "/opt/homebrew/lib/python3.14/site-packages".to_string(),
            )],
        })
    );
}

#[test]
fn parses_npm_show_details_from_plain_output() {
    let output = "\
requests@0.3.0 | MIT | deps: 7 | versions: 13
An streaming XHR abstraction that works in browsers and node.js
https://github.com/unshiftio/requests

keywords: request, xhr, requests
published over a year ago by swaagie <martijn@swaagman.online>
";

    assert_eq!(
        Backend::Npm.parse_show_details(output),
        Some(PackageDetails {
            backend: Backend::Npm,
            name: "requests".to_string(),
            version: "0.3.0".to_string(),
            summary: Some(
                "An streaming XHR abstraction that works in browsers and node.js".to_string()
            ),
            homepage: Some("https://github.com/unshiftio/requests".to_string()),
            license: Some("MIT".to_string()),
            author: Some("swaagie <martijn@swaagman.online>".to_string()),
            repository: None,
            keywords: vec![
                "request".to_string(),
                "xhr".to_string(),
                "requests".to_string(),
            ],
            dependencies: vec![],
            extra_fields: vec![(
                "Published".to_string(),
                "over a year ago by swaagie <martijn@swaagman.online>".to_string(),
            )],
        })
    );
}

#[test]
fn dedupes_search_candidates_per_backend() {
    let candidates = dedupe_search_candidates(vec![
        SearchCandidate {
            backend: Backend::Npm,
            label: "requests".to_string(),
            install_id: "requests".to_string(),
            version: Some("1.0.0".to_string()),
            source: Some("npm".to_string()),
        },
        SearchCandidate {
            backend: Backend::Npm,
            label: "requests".to_string(),
            install_id: "requests".to_string(),
            version: Some("1.0.1".to_string()),
            source: Some("npm".to_string()),
        },
        SearchCandidate {
            backend: Backend::Pip,
            label: "requests".to_string(),
            install_id: "requests".to_string(),
            version: None,
            source: Some("pypi".to_string()),
        },
    ]);

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Npm,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: Some("1.0.0".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Pip,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            }
        ]
    );
}

#[test]
fn installed_candidate_deduping_preserves_distinct_versions() {
    let candidates = dedupe_installed_candidates(vec![
        SearchCandidate {
            backend: Backend::Winget,
            label: "Quarto".to_string(),
            install_id: "Posit.Quarto".to_string(),
            version: Some("1.8.0".to_string()),
            source: None,
        },
        SearchCandidate {
            backend: Backend::Winget,
            label: "Quarto".to_string(),
            install_id: "Posit.Quarto".to_string(),
            version: Some("1.9.37".to_string()),
            source: None,
        },
        SearchCandidate {
            backend: Backend::Winget,
            label: "Quarto".to_string(),
            install_id: "Posit.Quarto".to_string(),
            version: Some("1.9.37".to_string()),
            source: None,
        },
    ]);

    assert_eq!(
        candidates,
        vec![
            SearchCandidate {
                backend: Backend::Winget,
                label: "Quarto".to_string(),
                install_id: "Posit.Quarto".to_string(),
                version: Some("1.8.0".to_string()),
                source: None,
            },
            SearchCandidate {
                backend: Backend::Winget,
                label: "Quarto".to_string(),
                install_id: "Posit.Quarto".to_string(),
                version: Some("1.9.37".to_string()),
                source: None,
            }
        ]
    );
}

#[test]
fn sorts_search_candidates_by_match_quality_then_backend_order() {
    let candidates = sort_search_candidates(
        vec![
            SearchCandidate {
                backend: Backend::Pip,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            },
            SearchCandidate {
                backend: Backend::Npm,
                label: "requests".to_string(),
                install_id: "requests".to_string(),
                version: Some("1.0.0".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Npm,
                label: "requests-cache".to_string(),
                install_id: "requests-cache".to_string(),
                version: Some("1.0.0".to_string()),
                source: Some("npm".to_string()),
            },
            SearchCandidate {
                backend: Backend::Pip,
                label: "python-requests-tools".to_string(),
                install_id: "python-requests-tools".to_string(),
                version: None,
                source: Some("pypi".to_string()),
            },
        ],
        "requests",
        &[Backend::Npm, Backend::Pip],
    );

    assert_eq!(
        candidates
            .iter()
            .map(|candidate| format!("{}:{}", candidate.backend, candidate.install_id))
            .collect::<Vec<_>>(),
        vec![
            "npm:requests".to_string(),
            "pip:requests".to_string(),
            "npm:requests-cache".to_string(),
            "pip:python-requests-tools".to_string(),
        ]
    );
}

#[test]
fn renders_backend_output_section_with_header() {
    assert_eq!(
        render_backend_output_section(Backend::Pip, "\nName: requests\nVersion: 2.32.0\n"),
        Some("== pip ==\nName: requests\nVersion: 2.32.0".to_string())
    );
}

#[test]
fn renders_multi_backend_package_details_as_comparison() {
    let rendered = render_package_details_sections(&[
        PackageDetails {
            backend: Backend::Npm,
            name: "pip".to_string(),
            version: "0.0.1".to_string(),
            summary: Some("Freckle CLI tool using node.js".to_string()),
            homepage: None,
            license: Some("Proprietary".to_string()),
            author: Some("sirkitree <sirkitree@gmail.com>".to_string()),
            repository: None,
            keywords: vec![],
            dependencies: vec!["optimist".to_string(), "freckle".to_string()],
            extra_fields: vec![(
                "Published".to_string(),
                "over a year ago by sirkitree <sirkitree@gmail.com>".to_string(),
            )],
        },
        PackageDetails {
            backend: Backend::Pip,
            name: "pip".to_string(),
            version: "26.0".to_string(),
            summary: Some("The PyPA recommended tool for installing Python packages.".to_string()),
            homepage: Some("https://pip.pypa.io/".to_string()),
            license: Some("MIT".to_string()),
            author: Some("The pip developers <distutils-sig@python.org>".to_string()),
            repository: None,
            keywords: vec![],
            dependencies: vec![],
            extra_fields: vec![(
                "Location".to_string(),
                "/opt/homebrew/lib/python3.14/site-packages".to_string(),
            )],
        },
    ]);

    assert_eq!(
        rendered,
        "\
== comparison ==
Name      : pip
Version   :
  npm      0.0.1
  pip      26.0
Summary   :
  npm      Freckle CLI tool using node.js
  pip      The PyPA recommended tool for installing Python packages.
Homepage  :
  pip      https://pip.pypa.io/
License   :
  npm      Proprietary
  pip      MIT
Author    :
  npm      sirkitree <sirkitree@gmail.com>
  pip      The pip developers <distutils-sig@python.org>
Depends On:
  npm      optimist, freckle

== npm extras ==
Published : over a year ago by sirkitree <sirkitree@gmail.com>

== pip extras ==
Location  : /opt/homebrew/lib/python3.14/site-packages"
            .to_string()
    );
}

#[test]
fn render_command_failure_prefers_stderr_details() {
    let invocation = Invocation::owned("pip", vec!["show".to_string(), "requests".to_string()]);
    let capture = CommandCapture {
        stdout: String::new(),
        stderr: "Package(s) not found".to_string(),
        success: false,
        status_code: 1,
        timed_out: false,
        timeout_seconds: None,
    };

    assert_eq!(
        render_command_failure(&invocation, &capture),
        "backend command failed with exit code 1: pip show requests (Package(s) not found)"
            .to_string()
    );
}

#[test]
fn npm_upgradable_list_accepts_exit_code_one() {
    let capture = CommandCapture {
        stdout: "{\"typescript\":{}}".to_string(),
        stderr: String::new(),
        success: false,
        status_code: 1,
        timed_out: false,
        timeout_seconds: None,
    };

    assert!(Backend::Npm.accepts_list_capture(true, &capture));
    assert!(!Backend::Npm.accepts_list_capture(false, &capture));
    assert!(!Backend::Pip.accepts_list_capture(true, &capture));
}

#[test]
fn pip_and_npm_hold_lives_in_waws_own_file() {
    // Neither backend can pin, so waw records the request instead. The
    // native planner must stay empty: producing an invocation here would
    // mean running a command that does not exist.
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: false,
        auto_elevate: true,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    assert!(Backend::Pip.uses_waw_holds());
    assert!(Backend::Npm.uses_waw_holds());
    assert!(!Backend::Winget.uses_waw_holds());
    assert!(!Backend::Scoop.uses_waw_holds());
    assert!(!Backend::Chocolatey.uses_waw_holds());

    assert!(
        Backend::Pip
            .plan_hold_pin(&["requests".to_string()], true, &runtime)
            .is_empty()
    );
}

#[test]
fn holds_round_trip_through_toml() {
    let mut holds = Holds::default();
    holds.insert(Backend::Pip, "requests".to_string());
    holds.insert(Backend::Pip, "django".to_string());
    holds.insert(Backend::Npm, "typescript".to_string());

    let rendered = holds.to_toml();
    let reparsed = Holds::parse(&rendered).expect("round trip should parse");

    assert_eq!(
        reparsed.packages(Backend::Pip),
        holds.packages(Backend::Pip)
    );
    assert_eq!(
        reparsed.packages(Backend::Pip),
        vec!["django".to_string(), "requests".to_string()],
        "packages should be stored in a stable order"
    );
    assert_eq!(
        reparsed.packages(Backend::Npm),
        vec!["typescript".to_string()]
    );
    assert!(reparsed.packages(Backend::Winget).is_empty());
}

#[test]
fn holds_insert_dedupes_case_insensitively() {
    let mut holds = Holds::default();
    holds.insert(Backend::Pip, "Requests".to_string());
    holds.insert(Backend::Pip, "requests".to_string());

    assert_eq!(holds.packages(Backend::Pip).len(), 1);
    assert!(holds.contains(Backend::Pip, "REQUESTS"));
}

#[test]
fn holds_remove_reports_whether_anything_changed() {
    let mut holds = Holds::default();
    holds.insert(Backend::Pip, "requests".to_string());

    assert!(holds.remove(Backend::Pip, "requests"));
    assert!(!holds.remove(Backend::Pip, "requests"));
    assert!(!holds.contains(Backend::Pip, "requests"));
}

#[test]
fn holds_reject_unknown_backend_keys() {
    let error = Holds::parse("conda = [\"numpy\"]\n").expect_err("should reject");
    assert!(error.contains("unsupported backend"), "{error}");
}

#[test]
fn held_upgrade_targets_are_filtered_for_pip_only() {
    let mut holds = Holds::default();
    holds.insert(Backend::Pip, "requests".to_string());
    holds.insert(Backend::Winget, "Git.Git".to_string());

    let (kept, skipped) = holds.filter_upgrade_targets(
        Backend::Pip,
        &["requests".to_string(), "flask==2.0".to_string()],
    );
    assert_eq!(kept, vec!["flask==2.0".to_string()]);
    assert_eq!(skipped, vec!["requests".to_string()]);

    // A native pin is enforced by the backend itself; filtering here would
    // let a stale holds.toml override a pin the user removed there.
    let (kept, skipped) = holds.filter_upgrade_targets(Backend::Winget, &["Git.Git".to_string()]);
    assert_eq!(kept, vec!["Git.Git".to_string()]);
    assert!(skipped.is_empty());
}

#[test]
fn upgrade_filter_strips_version_suffix_before_matching() {
    let mut holds = Holds::default();
    holds.insert(Backend::Pip, "requests".to_string());

    let (kept, skipped) =
        holds.filter_upgrade_targets(Backend::Pip, &["requests==2.31".to_string()]);
    assert!(kept.is_empty());
    assert_eq!(skipped, vec!["requests==2.31".to_string()]);
}

#[test]
fn winget_id_heuristic_recognises_dotted_ids() {
    assert!(looks_like_winget_id("Git.Git"));
    assert!(looks_like_winget_id("Microsoft.PowerToys"));
    assert!(!looks_like_winget_id("git"));
    assert!(!looks_like_winget_id("power toys"));
}

#[test]
fn resolving_a_winget_id_prefers_an_exact_id_then_name_then_suffix() {
    let entries = vec![
        PackageListEntry {
            backend: Backend::Winget,
            name: "Git".to_string(),
            package_id: "Git.Git".to_string(),
            current_version: "2.52.0".to_string(),
            available_version: None,
        },
        PackageListEntry {
            backend: Backend::Winget,
            name: "Microsoft Git".to_string(),
            package_id: "Microsoft.Git".to_string(),
            current_version: "2.56.0".to_string(),
            available_version: None,
        },
    ];

    assert_eq!(
        resolve_one_winget_id("Git.Git", &entries).expect("exact id"),
        "Git.Git"
    );
    assert_eq!(
        resolve_one_winget_id("Git", &entries).expect("exact name"),
        "Git.Git",
        "the exact display name wins over a suffix match"
    );
    assert_eq!(
        resolve_one_winget_id("git", &entries).expect("case-insensitive name"),
        "Git.Git"
    );
}

#[test]
fn resolving_an_ambiguous_winget_suffix_is_an_error() {
    let entries = vec![
        PackageListEntry {
            backend: Backend::Winget,
            name: "Git".to_string(),
            package_id: "Git.Git".to_string(),
            current_version: "2.52.0".to_string(),
            available_version: None,
        },
        PackageListEntry {
            backend: Backend::Winget,
            name: "Microsoft Git".to_string(),
            package_id: "Microsoft.Git".to_string(),
            current_version: "2.56.0".to_string(),
            available_version: None,
        },
    ];

    let error = resolve_one_winget_id("git-fork", &entries).expect_err("no match");
    assert!(
        error.contains("Git.Git") || error.contains("full id"),
        "{error}"
    );
}

#[test]
fn scoop_hold_list_is_parsed_from_the_info_column() {
    let output = concat!(
        "Installed apps:\r\n",
        "\r\n",
        "Name         Version      Source Updated             Info        \r\n",
        "----         -------      ------ -------             ----        \r\n",
        "aria2        1.37.0-1     main   2025-02-13 13:17:03             \r\n",
        "sudo         0.2020.01.26 main   2026-10-28 15:39:16 Held package\r\n",
    );

    assert_eq!(
        Backend::Scoop.parse_holds(output),
        vec![("sudo".to_string(), "Held package".to_string())]
    );
}

#[test]
fn scoop_hold_list_accepts_the_other_info_flags() {
    // A held global app reports two flags comma-joined.
    let output = concat!(
        "Name Version Source Updated Info\r\n",
        "---- ------- ------ ------- ----\r\n",
        "sudo 1.0     main   2026-01-01 Global install, Held package\r\n",
    );

    assert_eq!(
        Backend::Scoop.parse_holds(output),
        vec![("sudo".to_string(), "Held package".to_string())]
    );
}

#[test]
fn disabling_backend_clears_default_selection() {
    let mut config = Config {
        backend: Some(Backend::Npm),
        ..Config::default()
    };
    config.set_backend_enabled(Backend::Npm, false);

    assert!(!config.is_backend_enabled(Backend::Npm));
    assert_eq!(config.backend, None);
}

#[test]
fn backend_status_json_contains_key_fields() {
    let config = Config {
        backend: Some(Backend::Pip),
        enable_choco: false,
        ..Config::default()
    };

    let json = render_backend_statuses_json(&config);

    assert!(json.contains("\"backend\":\"pip\""));
    assert!(json.contains("\"default_selected\":true"));
    assert!(json.contains("\"backend\":\"choco\""));
    assert!(json.contains("\"enabled\":false"));
}

#[test]
fn finds_programs_on_path_using_explicit_file_names() {
    let base = env::temp_dir().join(format!(
        "waw-path-discovery-{}-{}",
        std::process::id(),
        APP_VERSION
    ));
    fs::create_dir_all(&base).expect("temp directory should exist");
    let program = base.join("winget.exe");
    fs::write(&program, b"").expect("program marker should be written");

    let joined = env::join_paths([base.as_path()]).expect("PATH should be buildable");
    let found = find_programs_on_path(Some(joined.as_os_str()), &["winget.exe"]);

    assert_eq!(found, vec![program.display().to_string()]);

    fs::remove_dir_all(&base).expect("temp directory should be removed");
}

#[test]
fn windows_apps_alias_path_without_real_file_is_not_discovered() {
    let base = env::temp_dir().join(format!(
        "waw-windowsapps-alias-{}-{}",
        std::process::id(),
        APP_VERSION
    ));
    let alias_dir = base.join("Microsoft").join("WindowsApps");
    fs::create_dir_all(&alias_dir).expect("alias directory should exist");

    let joined = env::join_paths([alias_dir.as_path()]).expect("PATH should be buildable");
    let found = find_programs_on_path(Some(joined.as_os_str()), &["winget.exe"]);

    assert!(found.is_empty());

    fs::remove_dir_all(&base).expect("temp directory should be removed");
}

#[test]
fn pip_probe_args_choose_python_module_mode_for_python_launchers() {
    assert_eq!(
        pip_probe_args(r"C:\Python313\python.exe"),
        vec!["-m".to_string(), "pip".to_string(), "--version".to_string()]
    );
    assert_eq!(
        pip_probe_args("py.exe"),
        vec!["-m".to_string(), "pip".to_string(), "--version".to_string()]
    );
    assert_eq!(
        pip_probe_args(r"C:\Python313\Scripts\pip.exe"),
        vec!["--version"]
    );
}

fn assert_program_matches(program: &str, expected_suffixes: &[&str]) {
    let normalized = program.replace('\\', "/").to_ascii_lowercase();
    assert!(
        expected_suffixes.iter().any(|suffix| {
            let suffix = suffix.replace('\\', "/").to_ascii_lowercase();
            normalized == suffix || normalized.ends_with(&format!("/{suffix}"))
        }),
        "program `{program}` did not match any expected suffix: {expected_suffixes:?}"
    );
}

fn utf16le_bytes(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

#[test]
fn show_results_json_contains_detail_fields() {
    let json = render_show_results_json(&[ShowBackendResult {
        backend: Backend::Pip,
        command: "python3 -m pip show pip".to_string(),
        success: true,
        dry_run: false,
        details: Some(PackageDetails {
            backend: Backend::Pip,
            name: "pip".to_string(),
            version: "26.0".to_string(),
            summary: Some("The PyPA recommended tool for installing Python packages.".to_string()),
            homepage: Some("https://pip.pypa.io/".to_string()),
            license: Some("MIT".to_string()),
            author: Some("The pip developers <distutils-sig@python.org>".to_string()),
            repository: None,
            keywords: vec![],
            dependencies: vec!["setuptools".to_string()],
            extra_fields: vec![(
                "Location".to_string(),
                "/opt/homebrew/lib/python3.14/site-packages".to_string(),
            )],
        }),
        raw_output: None,
        error: None,
    }]);

    assert!(json.contains("\"backend\":\"pip\""));
    assert!(json.contains("\"command\":\"python3 -m pip show pip\""));
    assert!(json.contains("\"details\":{"));
    assert!(json.contains("\"name\":\"pip\""));
    assert!(json.contains("\"dependencies\":[\"setuptools\"]"));
    assert!(json.contains(
        "\"extra_fields\":{\"Location\":\"/opt/homebrew/lib/python3.14/site-packages\"}"
    ));
    assert!(json.contains("\"error\":null"));
}

#[test]
fn show_results_json_can_encode_failures() {
    let json = render_show_results_json(&[ShowBackendResult {
        backend: Backend::Pip,
        command: "python3 -m pip show missing".to_string(),
        success: false,
        dry_run: false,
        details: None,
        raw_output: None,
        error: Some(
            "backend command failed with exit code 1: python3 -m pip show missing".to_string(),
        ),
    }]);

    assert!(json.contains("\"success\":false"));
    assert!(json.contains("\"details\":null"));
    assert!(json.contains("\"raw_output\":null"));
    assert!(json.contains(
        "\"error\":\"backend command failed with exit code 1: python3 -m pip show missing\""
    ));
}

#[test]
fn only_scoop_lacks_exact_version_pinning() {
    // `scoop install pkg@ver` rebuilds the manifest from the bucket's current
    // definition, so the version is a hint rather than a pin.
    assert!(!Backend::Scoop.version_pinning_is_exact());
    assert!(Backend::Winget.version_pinning_is_exact());
    assert!(Backend::Chocolatey.version_pinning_is_exact());
    assert!(Backend::Npm.version_pinning_is_exact());
    assert!(Backend::Pip.version_pinning_is_exact());
}

#[test]
fn scoop_versioned_install_still_passes_the_version_through() {
    // waw must not swallow the version: it is the best signal Scoop gets.
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let plan = Backend::Scoop.plan_install(&[PackageSpec::parse("gh@2.7.0")], &runtime);
    assert_eq!(plan.len(), 1);
    assert_eq!(
        plan[0].args,
        vec!["install".to_string(), "gh@2.7.0".to_string()]
    );
}

#[test]
fn unversioned_install_plans_are_unchanged_for_scoop() {
    let config = Config::default();
    let runtime = RuntimeSettings {
        assume_yes: true,
        auto_elevate: false,
        command_timeout: None,
        deep_search: false,
        config: &config,
    };

    let plan = Backend::Scoop.plan_install(&[PackageSpec::parse("gh")], &runtime);
    assert_eq!(plan[0].args, vec!["install".to_string(), "gh".to_string()]);
}

#[test]
fn elevation_in_process_requires_the_exact_value_one() {
    // Anything else must stay on the real elevation path: a typo here would
    // silently downgrade a privileged operation.
    assert!(!elevation_requested(None));
    assert!(!elevation_requested(Some("")));
    assert!(!elevation_requested(Some("0")));
    assert!(!elevation_requested(Some("true")));
    assert!(!elevation_requested(Some("yes")));
    assert!(elevation_requested(Some("1")));
    assert!(elevation_requested(Some(" 1 ")));
}

#[test]
fn backend_display_honours_formatter_width() {
    // `Display for Backend` used `f.write_str`, which ignores the formatter's
    // width and alignment. Every `{:<width$}` table silently lost its padding
    // for names shorter than the column, shifting the rest of the row left.
    assert_eq!(format!("[{:<8}]", Backend::Winget), "[winget  ]");
    assert_eq!(format!("[{:<8}]", Backend::Npm), "[npm     ]");
    assert_eq!(format!("[{:>8}]", Backend::Npm), "[     npm]");
    assert_eq!(format!("[{:<8}]", Backend::Chocolatey), "[choco   ]");
}

#[test]
fn backend_status_line_has_no_leading_padding() {
    // The live E2E gate parses this with `^\s*(?<name>\w+):.*available$`.
    // A padded status line (`    winget: ...`) broke that gate once already,
    // so the contract is asserted rather than left implicit.
    let status = BackendStatus {
        backend: Backend::Winget,
        supported: true,
        enabled: true,
        available: true,
        default_selected: false,
    };
    let line = render_backend_status_line(&status, "supported", "enabled", "available", "");
    assert_eq!(line, "winget: supported, enabled, available");
    assert!(!line.starts_with(' '), "leading padding breaks parsing");

    let line = render_backend_status_line(&status, "supported", "enabled", "available", " default");
    assert_eq!(line, "winget: supported, enabled, available default");
}
