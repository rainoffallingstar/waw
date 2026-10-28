//! Command-line parsing.
//!
//! A hand-written parser rather than clap, to keep the crate dependency-free.
//! Global flags are accepted before or after the subcommand name.

use crate::Backend;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cli {
    pub(crate) backend: Option<Backend>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
    pub(crate) assume_yes: Option<bool>,
    pub(crate) auto_elevate: Option<bool>,
    pub(crate) show_help: bool,
    pub(crate) show_version: bool,
    pub(crate) command: Subcommand,
}

impl Cli {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut parser = ArgCursor::new(args.into_iter().collect());
        let mut backend = None;
        let mut config_path = None;
        let mut dry_run = false;
        let mut json = false;
        let mut assume_yes = None;
        let mut auto_elevate = None;
        let mut show_help = false;
        let mut show_version = false;

        while let Some(arg) = parser.peek() {
            match arg {
                "--backend" | "-b" => {
                    parser.next();
                    let value = parser
                        .next()
                        .ok_or_else(|| "missing value for --backend".to_string())?;
                    backend = Some(Backend::parse(&value)?);
                }
                "--config" => {
                    parser.next();
                    let value = parser
                        .next()
                        .ok_or_else(|| "missing value for --config".to_string())?;
                    config_path = Some(PathBuf::from(value));
                }
                "--dry-run" => {
                    parser.next();
                    dry_run = true;
                }
                "--json" => {
                    parser.next();
                    json = true;
                }
                "-y" | "--yes" => {
                    parser.next();
                    assume_yes = Some(true);
                }
                "--interactive" => {
                    parser.next();
                    assume_yes = Some(false);
                }
                "--elevate" => {
                    parser.next();
                    auto_elevate = Some(true);
                }
                "--no-elevate" => {
                    parser.next();
                    auto_elevate = Some(false);
                }
                "-h" | "--help" => {
                    parser.next();
                    show_help = true;
                }
                "-V" | "--version" => {
                    parser.next();
                    show_version = true;
                }
                "help" => {
                    parser.next();
                    show_help = true;
                }
                _ if arg.starts_with('-') => {
                    return Err(format!("unknown global option: {arg}"));
                }
                _ => break,
            }
        }

        let command = if show_help || show_version {
            Subcommand::Help
        } else {
            let name = parser
                .next()
                .ok_or_else(|| "missing command. Try --help".to_string())?;

            // Global flags are also accepted after the subcommand name, so
            // `waw list --json` behaves like `waw --json list`. They are pulled
            // out before the subcommand parser sees them.
            let (sub_args, trailing_globals) = split_global_flags(parser.remaining());
            for flag in trailing_globals {
                match flag.as_str() {
                    "--json" => json = true,
                    "--dry-run" => dry_run = true,
                    "-y" | "--yes" => assume_yes = Some(true),
                    "--interactive" => assume_yes = Some(false),
                    "--elevate" => auto_elevate = Some(true),
                    "--no-elevate" => auto_elevate = Some(false),
                    "-h" | "--help" => show_help = true,
                    "-V" => show_version = true,
                    _ => {}
                }
            }
            let mut sub_parser = ArgCursor::new(sub_args);
            let parsed = Subcommand::parse(name, &mut sub_parser)?;
            if sub_parser.has_remaining() {
                return Err(format!(
                    "unexpected trailing arguments: {}",
                    sub_parser.remaining().join(" ")
                ));
            }
            parsed
        };

        Ok(Self {
            backend,
            config_path,
            dry_run,
            json,
            assume_yes,
            auto_elevate,
            show_help,
            show_version,
            command,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Subcommand {
    Update,
    Upgrade { packages: Vec<String> },
    Install { mode: InstallMode },
    Remove { mode: RemoveMode },
    Hold { packages: Vec<String>, enable: bool },
    Search { query: String },
    List { upgradable: bool },
    Show { package: String },
    Backends,
    Backend { action: BackendAction },
    /// `waw bucket [list]` -- inspect configured/available Scoop buckets.
    Bucket { action: BucketAction },
    Export { path: Option<PathBuf> },
    Import { path: PathBuf },
    Help,
}

/// A package request that may or may not pin a version.
///
/// `raw` keeps exactly what the user typed so diagnostics echo it back, while
/// `name`/`version` hold the split form the backends need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageSpec {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) raw: String,
}

impl PackageSpec {
    /// Splits `pkg@1.2.3` and `pkg==1.2.3` (pip style) into name and version.
    ///
    /// A leading `@` belongs to the scope of an npm package (`@scope/name`), so
    /// the separator search starts after it. The `==` form is checked first
    /// because pip uses it and a bare `@` inside a pip spec means something
    /// else entirely.
    pub(crate) fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();

        if let Some(index) = trimmed.find("==") {
            let (name, version) = trimmed.split_at(index);
            let version = &version[2..];
            if !name.is_empty() && !version.is_empty() {
                return Self {
                    name: name.to_string(),
                    version: Some(version.to_string()),
                    raw: trimmed.to_string(),
                };
            }
        }

        let search_from = usize::from(trimmed.starts_with('@'));
        if let Some(offset) = trimmed[search_from..].rfind('@') {
            let index = search_from + offset;
            let name = &trimmed[..index];
            let version = &trimmed[index + 1..];
            if !name.is_empty() && !version.is_empty() {
                return Self {
                    name: name.to_string(),
                    version: Some(version.to_string()),
                    raw: trimmed.to_string(),
                };
            }
        }

        Self {
            name: trimmed.to_string(),
            version: None,
            raw: trimmed.to_string(),
        }
    }

    pub(crate) fn without_version(raw: &str) -> Self {
        let parsed = Self::parse(raw);
        Self {
            name: parsed.name,
            version: None,
            raw: parsed.raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallMode {
    Search(String),
    Exact(Vec<PackageSpec>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoveMode {
    Search(String),
    Exact(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BucketAction {
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackendAction {
    List,
    Enable { backend: Backend },
    Disable { backend: Backend },
    Install { backend: Backend, enable: bool },
    Default { backend: Option<Backend> },
}

impl Subcommand {
    pub(crate) fn parse(name: String, parser: &mut ArgCursor) -> Result<Self, String> {
        match name.as_str() {
            "update" => Ok(Self::Update),
            "upgrade" => {
                let mut packages = Vec::new();
                let mut upgrade_all = false;

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--all" => {
                            parser.next();
                            upgrade_all = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for upgrade: {value}"));
                        }
                        _ => packages.push(parser.next().expect("peeked value must exist")),
                    }
                }

                if upgrade_all && !packages.is_empty() {
                    return Err("--all cannot be combined with explicit package names".to_string());
                }

                Ok(Self::Upgrade {
                    packages: if upgrade_all { Vec::new() } else { packages },
                })
            }
            "install" => {
                enum ParsedInstallMode {
                    Search,
                    Exact,
                }

                let mut mode = None;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--pick" | "--select" | "--interactive" => {
                            parser.next();
                            if matches!(mode, Some(ParsedInstallMode::Exact)) {
                                return Err(
                                    "install cannot combine search-selection flags with --exact"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedInstallMode::Search);
                        }
                        "--exact" | "--direct" => {
                            parser.next();
                            if matches!(mode, Some(ParsedInstallMode::Search)) {
                                return Err(
                                    "install cannot combine --exact with search-selection flags"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedInstallMode::Exact);
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for install: {value}"));
                        }
                        _ => break,
                    }
                }

                let mode = match mode.unwrap_or(ParsedInstallMode::Search) {
                    ParsedInstallMode::Search => {
                        InstallMode::Search(parser.take_required_text("install")?)
                    }
                    ParsedInstallMode::Exact => InstallMode::Exact(
                        parser
                            .take_required_packages("install --exact")?
                            .into_iter()
                            .map(|raw| PackageSpec::parse(&raw))
                            .collect(),
                    ),
                };

                Ok(Self::Install { mode })
            }
            "remove" => {
                enum ParsedRemoveMode {
                    Search,
                    Exact,
                }

                let mut mode = None;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--pick" | "--select" | "--interactive" => {
                            parser.next();
                            if matches!(mode, Some(ParsedRemoveMode::Exact)) {
                                return Err(
                                    "remove cannot combine search-selection flags with --exact"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedRemoveMode::Search);
                        }
                        "--exact" | "--direct" => {
                            parser.next();
                            if matches!(mode, Some(ParsedRemoveMode::Search)) {
                                return Err(
                                    "remove cannot combine --exact with search-selection flags"
                                        .to_string(),
                                );
                            }
                            mode = Some(ParsedRemoveMode::Exact);
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for remove: {value}"));
                        }
                        _ => break,
                    }
                }

                let mode = match mode.unwrap_or(ParsedRemoveMode::Search) {
                    ParsedRemoveMode::Search => {
                        RemoveMode::Search(parser.take_required_text("remove")?)
                    }
                    ParsedRemoveMode::Exact => {
                        RemoveMode::Exact(parser.take_required_packages("remove --exact")?)
                    }
                };

                Ok(Self::Remove { mode })
            }
            "hold" => {
                let mut enable = true;
                let mut packages = Vec::new();

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--off" | "--unhold" => {
                            parser.next();
                            enable = false;
                        }
                        "--on" => {
                            parser.next();
                            enable = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for hold: {value}"));
                        }
                        _ => packages.push(parser.next().expect("peeked value must exist")),
                    }
                }

                if packages.is_empty() {
                    return Err("hold requires at least one package".to_string());
                }

                Ok(Self::Hold { packages, enable })
            }
            "search" => Ok(Self::Search {
                query: parser.take_required_text("search")?,
            }),
            "list" => {
                let mut upgradable = false;

                while let Some(arg) = parser.peek() {
                    match arg {
                        "--upgradable" | "--upgradeable" | "--updates" => {
                            parser.next();
                            upgradable = true;
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for list: {value}"));
                        }
                        _ => {
                            return Err(format!(
                                "list does not accept positional arguments: {}",
                                parser.remaining().join(" ")
                            ));
                        }
                    }
                }

                Ok(Self::List { upgradable })
            }
            "show" => Ok(Self::Show {
                package: parser.take_required_single_value("show")?,
            }),
            "export" => {
                let mut path = None;
                while let Some(arg) = parser.peek() {
                    let arg = arg.to_string();
                    match arg.as_str() {
                        "--file" | "-o" => {
                            parser.next();
                            let value = parser
                                .next()
                                .ok_or_else(|| "export --file requires a path".to_string())?;
                            path = Some(PathBuf::from(value));
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for export: {value}"));
                        }
                        _ => {
                            let value = parser.next().expect("peeked value must exist");
                            path = Some(PathBuf::from(value));
                        }
                    }
                }
                Ok(Self::Export { path })
            }
            "import" => {
                let mut path = None;
                while let Some(arg) = parser.peek() {
                    let arg = arg.to_string();
                    match arg.as_str() {
                        "--file" | "-f" => {
                            parser.next();
                            let value = parser
                                .next()
                                .ok_or_else(|| "import --file requires a path".to_string())?;
                            path = Some(PathBuf::from(value));
                        }
                        value if value.starts_with('-') => {
                            return Err(format!("unknown option for import: {value}"));
                        }
                        _ => {
                            if path.is_some() {
                                return Err(
                                    "import accepts only one manifest path".to_string()
                                );
                            }
                            path = Some(PathBuf::from(
                                parser.next().expect("peeked value must exist"),
                            ));
                        }
                    }
                }
                let path = path.ok_or_else(|| {
                    "import requires a manifest path (or --file <path>)".to_string()
                })?;
                Ok(Self::Import { path })
            }
            "backends" => Ok(Self::Backends),
            "bucket" => {
                let action = parser.next().unwrap_or_else(|| "list".to_string());
                match action.as_str() {
                    "list" => {
                        if parser.has_remaining() {
                            return Err(format!(
                                "bucket list does not accept extra arguments: {}",
                                parser.remaining().join(" ")
                            ));
                        }
                        Ok(Self::Bucket {
                            action: BucketAction::List,
                        })
                    }
                    other => Err(format!("unsupported bucket command: {other}")),
                }
            }
            "backend" => Ok(Self::Backend {
                action: BackendAction::parse(parser)?,
            }),
            other => Err(format!("unsupported command: {other}")),
        }
    }
}

impl BackendAction {
    pub(crate) fn parse(parser: &mut ArgCursor) -> Result<Self, String> {
        let action = parser.next().ok_or_else(|| {
            "backend requires a subcommand: list, enable, disable, install, default".to_string()
        })?;

        match action.as_str() {
            "list" => {
                if parser.has_remaining() {
                    return Err(format!(
                        "backend list does not accept extra arguments: {}",
                        parser.remaining().join(" ")
                    ));
                }
                Ok(Self::List)
            }
            "enable" => Ok(Self::Enable {
                backend: Backend::parse(&parser.take_required_single_value("backend enable")?)?,
            }),
            "disable" => Ok(Self::Disable {
                backend: Backend::parse(&parser.take_required_single_value("backend disable")?)?,
            }),
            "install" => {
                let mut enable = false;
                let backend_name = parser
                    .next()
                    .ok_or_else(|| "backend install requires exactly one backend".to_string())?;
                while let Some(arg) = parser.peek() {
                    match arg {
                        "--enable" => {
                            parser.next();
                            enable = true;
                        }
                        value => {
                            return Err(format!("unknown option for backend install: {value}"));
                        }
                    }
                }
                Ok(Self::Install {
                    backend: Backend::parse(&backend_name)?,
                    enable,
                })
            }
            "default" => {
                let value = parser.next().ok_or_else(|| {
                    "backend default requires a backend name or `auto`".to_string()
                })?;
                if parser.has_remaining() {
                    return Err(format!(
                        "backend default does not accept extra arguments: {}",
                        parser.remaining().join(" ")
                    ));
                }
                Ok(Self::Default {
                    backend: if value.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(Backend::parse(&value)?)
                    },
                })
            }
            other => Err(format!("unsupported backend subcommand: {other}")),
        }
    }
}

/// Options that are global but commonly typed after the subcommand, e.g.
/// `waw list --json`. This collects them out of the subcommand argument list.
///
/// The `--file`/`-f`/`-o` options take a value, so the token after them is left
/// alone; otherwise `waw export --file --json` would swallow a real path.
pub(crate) fn split_global_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    const VALUE_TAKING: [&str; 5] = ["--file", "-o", "-f", "--config", "--backend"];
    const GLOBAL: [&str; 10] = [
        "--json",
        "--dry-run",
        "-y",
        "--yes",
        "--interactive",
        "--elevate",
        "--no-elevate",
        "-h",
        "--help",
        "-V",
    ];

    let mut remaining = Vec::with_capacity(args.len());
    let mut globals = Vec::new();
    let mut skip_next_value = false;

    for arg in args {
        if skip_next_value {
            skip_next_value = false;
            remaining.push(arg.clone());
            continue;
        }
        if VALUE_TAKING.contains(&arg.as_str()) {
            skip_next_value = true;
            remaining.push(arg.clone());
            continue;
        }
        if GLOBAL.contains(&arg.as_str()) {
            globals.push(arg.clone());
            continue;
        }
        remaining.push(arg.clone());
    }

    (remaining, globals)
}

#[derive(Debug, Clone)]
pub(crate) struct ArgCursor {
    pub(crate) args: Vec<String>,
    pub(crate) index: usize,
}

impl ArgCursor {
    pub(crate) fn new(args: Vec<String>) -> Self {
        Self { args, index: 0 }
    }

    pub(crate) fn peek(&self) -> Option<&str> {
        self.args.get(self.index).map(String::as_str)
    }

    pub(crate) fn next(&mut self) -> Option<String> {
        let item = self.args.get(self.index).cloned();
        if item.is_some() {
            self.index += 1;
        }
        item
    }

    pub(crate) fn has_remaining(&self) -> bool {
        self.index < self.args.len()
    }

    pub(crate) fn remaining(&self) -> &[String] {
        &self.args[self.index..]
    }

    pub(crate) fn take_required_packages(&mut self, command: &str) -> Result<Vec<String>, String> {
        let mut packages = Vec::new();

        while let Some(arg) = self.peek() {
            if arg.starts_with('-') {
                return Err(format!("unknown option for {command}: {arg}"));
            }
            packages.push(self.next().expect("peeked value must exist"));
        }

        if packages.is_empty() {
            Err(format!("{command} requires at least one package"))
        } else {
            Ok(packages)
        }
    }

    pub(crate) fn take_required_text(&mut self, command: &str) -> Result<String, String> {
        let mut values = Vec::new();

        while let Some(arg) = self.peek() {
            if arg.starts_with('-') {
                return Err(format!("unknown option for {command}: {arg}"));
            }
            values.push(self.next().expect("peeked value must exist"));
        }

        if values.is_empty() {
            Err(format!("{command} requires a query"))
        } else {
            Ok(values.join(" "))
        }
    }

    pub(crate) fn take_required_single_value(&mut self, command: &str) -> Result<String, String> {
        let values = self.take_required_packages(command)?;
        if values.len() != 1 {
            Err(format!("{command} requires exactly one package"))
        } else {
            Ok(values.into_iter().next().expect("length already checked"))
        }
    }
}
