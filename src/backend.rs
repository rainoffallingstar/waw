//! Backend detection and command planning.
//!
//! `Backend` is the enum every command dispatches through. Each variant
//! knows how to locate its tool, probe availability, and translate a
//! `Subcommand` into concrete `Invocation`s.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Winget,
    Scoop,
    Chocolatey,
    Npm,
    Pip,
}

impl Backend {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "winget" => Ok(Self::Winget),
            "scoop" => Ok(Self::Scoop),
            "choco" | "chocolatey" => Ok(Self::Chocolatey),
            "npm" => Ok(Self::Npm),
            "pip" => Ok(Self::Pip),
            _ => Err(format!(
                "unsupported backend: {value}. Expected one of: winget, scoop, choco, npm, pip"
            )),
        }
    }

    pub(crate) fn command_override_env(self) -> &'static str {
        match self {
            Self::Winget => "WAW_WINGET_CMD",
            Self::Scoop => "WAW_SCOOP_CMD",
            Self::Chocolatey => "WAW_CHOCO_CMD",
            Self::Npm => "WAW_NPM_CMD",
            Self::Pip => "WAW_PYTHON_CMD",
        }
    }

    pub(crate) fn command_override(self) -> Option<String> {
        env_override(self.command_override_env())
    }

    pub(crate) fn detect(config: &Config) -> Result<Self, String> {
        let candidates = [
            Self::Winget,
            Self::Scoop,
            Self::Chocolatey,
            Self::Npm,
            Self::Pip,
        ];
        candidates
            .into_iter()
            .find(|backend| config.is_backend_enabled(*backend) && backend.is_available())
            .ok_or_else(|| {
                "no enabled backend was found in PATH. Enable a backend or install winget, scoop, choco, npm, or pip.".to_string()
            })
    }

    pub(crate) fn is_available(self) -> bool {
        self.command_candidates()
            .into_iter()
            .any(|(program, args)| command_works_or_is_discoverable(&program, &args))
    }

    pub(crate) fn command_candidates(self) -> Vec<(String, Vec<String>)> {
        match self {
            Self::Winget | Self::Scoop | Self::Chocolatey | Self::Npm => self
                .discovered_program_candidates()
                .into_iter()
                .map(|program| (program, self.version_probe_args()))
                .collect(),
            Self::Pip => self
                .discovered_pip_candidates()
                .into_iter()
                .map(|program| (program.clone(), pip_probe_args(&program)))
                .collect(),
        }
    }

    pub(crate) fn supported_on_host(self) -> bool {
        match self {
            Self::Winget | Self::Scoop | Self::Chocolatey => cfg!(windows),
            Self::Npm | Self::Pip => true,
        }
    }

    pub(crate) fn supports_bootstrap_install_on_host(self) -> bool {
        match self {
            Self::Scoop | Self::Chocolatey => cfg!(windows),
            Self::Npm | Self::Pip => cfg!(windows),
            Self::Winget => false,
        }
    }

    pub(crate) fn install_hint(self) -> &'static str {
        match self {
            Self::Winget => {
                "Install Microsoft App Installer / WinGet from the Microsoft Store or official package manager distribution."
            }
            Self::Scoop => "Install Scoop using the official quickstart at https://scoop.sh/",
            Self::Chocolatey => {
                "Install Chocolatey using the official guide at https://chocolatey.org/install"
            }
            Self::Npm => "Install Node.js, for example: winget install OpenJS.NodeJS.LTS",
            Self::Pip => "Install Python, for example: winget install Python.Python.3.12",
        }
    }

    pub(crate) fn install_invocation(self, assume_yes: bool) -> Option<Invocation> {
        if !self.supports_bootstrap_install_on_host() {
            return None;
        }

        match self {
            Self::Winget => None,
            Self::Scoop | Self::Chocolatey => None,
            Self::Npm => {
                let mut args = vec![
                    "install".to_string(),
                    "--id".to_string(),
                    "OpenJS.NodeJS.LTS".to_string(),
                    "--exact".to_string(),
                    "--accept-source-agreements".to_string(),
                    "--accept-package-agreements".to_string(),
                ];
                if assume_yes {
                    args.push("--silent".to_string());
                    args.push("--disable-interactivity".to_string());
                }
                Some(
                    Backend::Winget
                        .base_invocation(args)
                        .with_elevation(cfg!(windows)),
                )
            }
            Self::Pip => {
                let mut args = vec![
                    "install".to_string(),
                    "--id".to_string(),
                    "Python.Python.3.12".to_string(),
                    "--exact".to_string(),
                    "--accept-source-agreements".to_string(),
                    "--accept-package-agreements".to_string(),
                ];
                if assume_yes {
                    args.push("--silent".to_string());
                    args.push("--disable-interactivity".to_string());
                }
                Some(
                    Backend::Winget
                        .base_invocation(args)
                        .with_elevation(cfg!(windows)),
                )
            }
        }
    }

    pub(crate) fn base_invocation(self, args: Vec<String>) -> Invocation {
        match self {
            Self::Winget | Self::Scoop | Self::Chocolatey | Self::Npm => {
                let program = self.preferred_program();
                Invocation::owned(&program, args)
            }
            Self::Pip => {
                let program = self.pip_python_program();
                let mut pip_args = vec!["-m".to_string(), "pip".to_string()];
                pip_args.extend(args);
                Invocation::owned(&program, pip_args)
            }
        }
    }

    pub(crate) fn mutating_invocation(self, args: Vec<String>, runtime: &RuntimeSettings<'_>) -> Invocation {
        self.base_invocation(args)
            .with_elevation(self.requires_elevation_for_mutation(runtime))
    }

    /// Whether a mutating command genuinely needs administrator rights.
    ///
    /// Only Chocolatey installs machine-wide by default. winget, Scoop and npm
    /// all install per-user, and pip runs with `--user` unless configured
    /// otherwise, so treating them as elevated made every `waw install` raise
    /// a UAC prompt for no reason.
    pub(crate) fn requires_elevation_for_mutation(self, runtime: &RuntimeSettings<'_>) -> bool {
        if !cfg!(windows) {
            return false;
        }

        match self {
            Self::Chocolatey => true,
            Self::Winget | Self::Scoop | Self::Npm => false,
            Self::Pip => !runtime.config.pip_user,
        }
    }

    pub(crate) fn plan(
        self,
        command: &Subcommand,
        runtime: &RuntimeSettings<'_>,
    ) -> Result<Vec<Invocation>, String> {
        match command {
            Subcommand::Update => Ok(self.plan_update(runtime)),
            Subcommand::Upgrade { packages } => Ok(self.plan_upgrade(packages, runtime)),
            Subcommand::Install {
                mode: InstallMode::Exact(packages),
            } => Ok(self.plan_install(packages, runtime)),
            Subcommand::Install {
                mode: InstallMode::Search(_),
            } => Ok(Vec::new()),
            Subcommand::Remove {
                mode: RemoveMode::Search(_),
            } => Ok(Vec::new()),
            Subcommand::Remove {
                mode: RemoveMode::Exact(packages),
            } => Ok(self.plan_remove(
                &packages
                    .iter()
                    .cloned()
                    .map(RemoveTarget::unversioned)
                    .collect::<Vec<_>>(),
                runtime,
            )),
            Subcommand::Hold {
                packages,
                enable,
                list: _,
            } => {
                // `hold --list` is handled in main.rs before reaching here.
                Ok(self.plan_hold(packages, *enable, runtime))
            }
            Subcommand::Search { query, .. } => Ok(self.plan_search(query, runtime)),
            Subcommand::List { upgradable } => Ok(self.plan_list(*upgradable, runtime)),
            Subcommand::Show { package } => Ok(self.plan_show(package, runtime)),
            Subcommand::Backends | Subcommand::Backend { action: _ } => Ok(Vec::new()),
            Subcommand::Export { .. }
            | Subcommand::Import { .. }
            | Subcommand::Bucket { .. } => Ok(Vec::new()),
            Subcommand::Help => Ok(Vec::new()),
        }
    }

    pub(crate) fn plan_update(self, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                let mut args = vec!["source".to_string(), "update".to_string()];
                if let Some(source) = runtime.config.winget_source() {
                    args.push("--name".to_string());
                    args.push(source.to_string());
                }
                vec![self.mutating_invocation(args, runtime)]
            }
            Self::Scoop => vec![self.mutating_invocation(vec!["update".to_string()], runtime)],
            Self::Chocolatey => vec![Invocation::message(
                "Chocolatey does not expose a dedicated apt-get-style index refresh. Skipping update as a no-op.",
            )],
            Self::Npm => vec![Invocation::message(
                "npm does not expose a separate apt-get-style update step. Skipping update as a no-op.",
            )],
            Self::Pip => vec![Invocation::message(
                "pip does not expose a separate apt-get-style update step. Skipping update as a no-op.",
            )],
        }
    }

    pub(crate) fn plan_upgrade(self, packages: &[String], runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                if packages.is_empty() {
                    let mut args = vec![
                        "upgrade".to_string(),
                        "--all".to_string(),
                        "--include-unknown".to_string(),
                        "--accept-source-agreements".to_string(),
                        "--accept-package-agreements".to_string(),
                    ];
                    if runtime.assume_yes {
                        args.push("--silent".to_string());
                        args.push("--disable-interactivity".to_string());
                    }
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.mutating_invocation(args, runtime)]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            let mut args = vec![
                                "upgrade".to_string(),
                                "--id".to_string(),
                                package.clone(),
                                "--exact".to_string(),
                                "--accept-source-agreements".to_string(),
                                "--accept-package-agreements".to_string(),
                            ];
                            append_source_arg(&mut args, runtime.config.winget_source());
                            if runtime.assume_yes {
                                args.push("--silent".to_string());
                                args.push("--disable-interactivity".to_string());
                            }
                            self.mutating_invocation(args, runtime)
                        })
                        .collect()
                }
            }
            Self::Scoop => {
                if packages.is_empty() {
                    vec![
                        self.mutating_invocation(
                            vec!["update".to_string(), "*".to_string()],
                            runtime,
                        ),
                    ]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            self.mutating_invocation(
                                vec![
                                    "update".to_string(),
                                    runtime.config.qualify_scoop_package(package),
                                ],
                                runtime,
                            )
                        })
                        .collect()
                }
            }
            Self::Chocolatey => {
                if packages.is_empty() {
                    let mut args = vec!["upgrade".to_string(), "all".to_string()];
                    append_source_arg(&mut args, runtime.config.choco_source());
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    vec![self.mutating_invocation(args, runtime)]
                } else {
                    packages
                        .iter()
                        .map(|package| {
                            let mut args = vec!["upgrade".to_string(), package.clone()];
                            append_source_arg(&mut args, runtime.config.choco_source());
                            if runtime.assume_yes {
                                args.push("-y".to_string());
                            }
                            self.mutating_invocation(args, runtime)
                        })
                        .collect()
                }
            }
            Self::Npm => {
                if packages.is_empty() {
                    vec![self.mutating_invocation(
                        vec!["update".to_string(), "--global".to_string()],
                        runtime,
                    )]
                } else {
                    let specs = upgrade_specs(packages);
                    self.plan_install_or_upgrade(&specs, runtime, true)
                }
            }
            Self::Pip => {
                let specs = upgrade_specs(packages);
                self.plan_install_or_upgrade(&specs, runtime, true)
            }
        }
    }

    pub(crate) fn plan_install(
        self,
        packages: &[PackageSpec],
        runtime: &RuntimeSettings<'_>,
    ) -> Vec<Invocation> {
        self.plan_install_or_upgrade(packages, runtime, false)
    }

    pub(crate) fn plan_install_or_upgrade(
        self,
        packages: &[PackageSpec],
        runtime: &RuntimeSettings<'_>,
        upgrade: bool,
    ) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        if upgrade {
                            "upgrade".to_string()
                        } else {
                            "install".to_string()
                        },
                        "--id".to_string(),
                        package.name.clone(),
                        "--exact".to_string(),
                        "--accept-source-agreements".to_string(),
                        "--accept-package-agreements".to_string(),
                    ];
                    if let Some(version) = package.version.as_deref() {
                        args.push("--version".to_string());
                        args.push(version.to_string());
                    }
                    append_source_arg(&mut args, runtime.config.winget_source());
                    if runtime.assume_yes {
                        args.push("--silent".to_string());
                        args.push("--disable-interactivity".to_string());
                    }
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    let qualified = runtime.config.qualify_scoop_package(&package.name);
                    let target = match package.version.as_deref() {
                        Some(version) => format!("{qualified}@{version}"),
                        None => qualified,
                    };
                    self.mutating_invocation(
                        vec![
                            if upgrade {
                                "update".to_string()
                            } else {
                                "install".to_string()
                            },
                            target,
                        ],
                        runtime,
                    )
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        if upgrade {
                            "upgrade".to_string()
                        } else {
                            "install".to_string()
                        },
                        package.name.clone(),
                    ];
                    if let Some(version) = package.version.as_deref() {
                        args.push("--version".to_string());
                        args.push(version.to_string());
                    }
                    append_source_arg(&mut args, runtime.config.choco_source());
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
            Self::Npm => packages
                .iter()
                .map(|package| {
                    let versioned = match (upgrade, package.version.as_deref()) {
                        (_, Some(version)) => format!("{}@{version}", package.name),
                        (true, None) => format!("{}@latest", package.name),
                        (false, None) => package.name.clone(),
                    };
                    self.mutating_invocation(
                        vec!["install".to_string(), versioned, "--global".to_string()],
                        runtime,
                    )
                })
                .collect(),
            Self::Pip => packages
                .iter()
                .map(|package| {
                    let spec = match package.version.as_deref() {
                        Some(version) => format!("{}=={version}", package.name),
                        None => package.name.clone(),
                    };
                    let mut args = vec![
                        "install".to_string(),
                        spec,
                        "--no-input".to_string(),
                    ];
                    if upgrade {
                        args.push("--upgrade".to_string());
                    }
                    if runtime.config.pip_user {
                        args.push("--user".to_string());
                    }
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
        }
    }

    pub(crate) fn plan_remove(
        self,
        packages: &[RemoveTarget],
        runtime: &RuntimeSettings<'_>,
    ) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let mut args = vec![
                        "uninstall".to_string(),
                        "--id".to_string(),
                        package.package.clone(),
                        "--exact".to_string(),
                    ];
                    if let Some(version) = package.version.as_deref() {
                        args.push("--version".to_string());
                        args.push(version.to_string());
                    }
                    append_source_arg(&mut args, runtime.config.winget_source());
                    if runtime.assume_yes {
                        args.push("--silent".to_string());
                        args.push("--disable-interactivity".to_string());
                    }
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    self.mutating_invocation(
                        vec![
                            "uninstall".to_string(),
                            runtime.config.qualify_scoop_package(&package.package),
                        ],
                        runtime,
                    )
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    let mut args = vec!["uninstall".to_string(), package.package.clone()];
                    if runtime.assume_yes {
                        args.push("-y".to_string());
                    }
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
            Self::Npm => packages
                .iter()
                .map(|package| {
                    self.mutating_invocation(
                        vec![
                            "uninstall".to_string(),
                            package.package.clone(),
                            "--global".to_string(),
                        ],
                        runtime,
                    )
                })
                .collect(),
            Self::Pip => packages
                .iter()
                .map(|package| {
                    self.mutating_invocation(
                        vec![
                            "uninstall".to_string(),
                            package.package.clone(),
                            "--no-input".to_string(),
                            "--yes".to_string(),
                        ],
                        runtime,
                    )
                })
                .collect(),
        }
    }

    /// The command that reports this backend's pinned packages, or `None` when
    /// the backend has no way to enumerate them.
    ///
    /// Scoop is the awkward one: `scoop hold` only *sets* a hold and there is
    /// no listing subcommand, so this returns None rather than guessing.
    pub(crate) fn plan_hold_list(self) -> Option<Invocation> {
        match self {
            Self::Winget | Self::Chocolatey => Some(self.base_invocation(vec![
                "pin".to_string(),
                "list".to_string(),
            ])),
            Self::Scoop | Self::Npm | Self::Pip => None,
        }
    }

    pub(crate) fn plan_hold(
        self,
        packages: &[String],
        enable: bool,
        runtime: &RuntimeSettings<'_>,
    ) -> Vec<Invocation> {
        match self {
            Self::Winget => packages
                .iter()
                .map(|package| {
                    let action = if enable { "add" } else { "remove" };
                    let mut args = vec![
                        "pin".to_string(),
                        action.to_string(),
                        "--id".to_string(),
                        package.clone(),
                    ];
                    if enable {
                        args.push("--blocking".to_string());
                    }
                    args.push("--exact".to_string());
                    self.mutating_invocation(args, runtime)
                })
                .collect(),
            Self::Scoop => packages
                .iter()
                .map(|package| {
                    self.mutating_invocation(
                        vec![
                            if enable { "hold" } else { "unhold" }.to_string(),
                            package.clone(),
                        ],
                        runtime,
                    )
                })
                .collect(),
            Self::Chocolatey => packages
                .iter()
                .map(|package| {
                    self.mutating_invocation(
                        vec![
                            "pin".to_string(),
                            if enable { "add" } else { "remove" }.to_string(),
                            "--name".to_string(),
                            package.clone(),
                        ],
                        runtime,
                    )
                })
                .collect(),
            Self::Npm | Self::Pip => vec![Invocation::message(
                "This backend does not support a native hold/pin operation.",
            )],
        }
    }

    pub(crate) fn plan_search(self, query: &str, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        vec![self.search_invocation(query, runtime)]
    }

    pub(crate) fn search_invocation(self, query: &str, runtime: &RuntimeSettings<'_>) -> Invocation {
        match self {
            Self::Winget => {
                let mut args = vec!["search".to_string(), query.to_string()];
                append_source_arg(&mut args, runtime.config.winget_source());
                self.base_invocation(args)
            }
            Self::Scoop => self.base_invocation(vec!["search".to_string(), query.to_string()]),
            Self::Chocolatey => {
                let mut args = vec![
                    "search".to_string(),
                    query.to_string(),
                    "--limit-output".to_string(),
                ];
                append_source_arg(&mut args, runtime.config.choco_source());
                self.base_invocation(args)
            }
            Self::Npm => self.base_invocation(vec![
                "search".to_string(),
                query.to_string(),
                "--json".to_string(),
            ]),
            Self::Pip => self.pip_search_invocation(query, runtime.deep_search),
        }
    }

    pub(crate) fn parse_search_candidates(self, output: &str) -> Vec<SearchCandidate> {
        let mut candidates = match self {
            Self::Winget => parse_winget_search_candidates(output),
            Self::Scoop => parse_scoop_search_candidates(output),
            Self::Chocolatey => parse_choco_search_candidates(output),
            Self::Npm => parse_npm_search_candidates(output),
            Self::Pip => parse_pip_search_candidates(output),
        };
        for candidate in &mut candidates {
            candidate.backend = self;
        }
        candidates
    }

    pub(crate) fn plan_list(self, upgradable: bool, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                if upgradable {
                    let mut args = vec!["upgrade".to_string()];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.base_invocation(args)]
                } else {
                    let mut args = vec!["list".to_string()];
                    append_source_arg(&mut args, runtime.config.winget_source());
                    vec![self.base_invocation(args)]
                }
            }
            Self::Scoop => vec![self.base_invocation(vec![if upgradable {
                "status".to_string()
            } else {
                "list".to_string()
            }])],
            Self::Chocolatey => {
                let mut args = if upgradable {
                    vec!["outdated".to_string(), "--limit-output".to_string()]
                } else {
                    vec![
                        "list".to_string(),
                        "--local-only".to_string(),
                        "--limit-output".to_string(),
                    ]
                };
                append_source_arg(&mut args, runtime.config.choco_source());
                vec![self.base_invocation(args)]
            }
            Self::Npm => vec![self.base_invocation(if upgradable {
                vec![
                    "outdated".to_string(),
                    "--json".to_string(),
                    "--global".to_string(),
                ]
            } else {
                vec![
                    "list".to_string(),
                    "--json".to_string(),
                    "--depth=0".to_string(),
                    "--global".to_string(),
                ]
            })],
            Self::Pip => vec![self.base_invocation(if upgradable {
                vec![
                    "list".to_string(),
                    "--outdated".to_string(),
                    "--format=json".to_string(),
                ]
            } else {
                vec!["list".to_string(), "--format=json".to_string()]
            })],
        }
    }

    pub(crate) fn accepts_list_capture(self, upgradable: bool, capture: &CommandCapture) -> bool {
        capture.success || matches!(self, Self::Npm if upgradable && capture.status_code == 1)
    }

    pub(crate) fn parse_list_entries(self, upgradable: bool, output: &str) -> Option<Vec<PackageListEntry>> {
        match (self, upgradable) {
            (Self::Winget, false) => parse_winget_list_entries(output),
            (Self::Winget, true) => parse_winget_upgrade_entries(output),
            (Self::Scoop, false) => parse_scoop_list_entries(output),
            (Self::Scoop, true) => parse_scoop_upgrade_entries(output),
            (Self::Chocolatey, false) => parse_choco_list_entries(output),
            (Self::Chocolatey, true) => parse_choco_upgrade_entries(output),
            (Self::Npm, false) => parse_npm_list_entries(output),
            (Self::Npm, true) => parse_npm_upgrade_entries(output),
            (Self::Pip, false) => parse_pip_list_entries(output),
            (Self::Pip, true) => parse_pip_upgrade_entries(output),
        }
    }

    pub(crate) fn parse_show_details(self, output: &str) -> Option<PackageDetails> {
        match self {
            Self::Npm => parse_npm_show_details(output),
            Self::Pip => parse_pip_show_details(output),
            Self::Chocolatey => parse_choco_show_details(output),
            Self::Winget | Self::Scoop => parse_key_value_show_details(output)
                .map(|mut details| {
                    details.backend = self;
                    details
                }),
        }
    }

    pub(crate) fn plan_show(self, package: &str, runtime: &RuntimeSettings<'_>) -> Vec<Invocation> {
        match self {
            Self::Winget => {
                let mut args = vec![
                    "show".to_string(),
                    "--id".to_string(),
                    package.to_string(),
                    "--exact".to_string(),
                ];
                append_source_arg(&mut args, runtime.config.winget_source());
                vec![self.base_invocation(args)]
            }
            Self::Scoop => vec![self.base_invocation(vec![
                "info".to_string(),
                runtime.config.qualify_scoop_package(package),
            ])],
            Self::Chocolatey => {
                let mut args = vec!["info".to_string(), package.to_string()];
                append_source_arg(&mut args, runtime.config.choco_source());
                vec![self.base_invocation(args)]
            }
            Self::Npm => vec![self.base_invocation(vec!["view".to_string(), package.to_string()])],
            Self::Pip => vec![self.base_invocation(vec!["show".to_string(), package.to_string()])],
        }
    }

    pub(crate) fn pip_search_invocation(self, query: &str, deep: bool) -> Invocation {
        let python = self.pip_python_program();
        let mut args = vec![
            "-c".to_string(),
            PIP_SEARCH_SCRIPT.to_string(),
            query.to_string(),
        ];
        if deep {
            args.push("--deep".to_string());
            args.push(pip_index_cache_path().to_string_lossy().into_owned());
        }
        Invocation::owned(&python, args)
    }

    pub(crate) fn pip_python_program(self) -> String {
        if let Some(program) = self.command_override() {
            return program;
        }

        if let Some(program) = self
            .discovered_pip_candidates()
            .into_iter()
            .find(|program| is_python_launcher(program))
        {
            return program;
        }

        if command_works(
            "python",
            &["-m".to_string(), "pip".to_string(), "--version".to_string()],
        ) {
            "python".to_string()
        } else if command_works(
            "python3",
            &["-m".to_string(), "pip".to_string(), "--version".to_string()],
        ) {
            "python3".to_string()
        } else {
            "python".to_string()
        }
    }

    pub(crate) fn preferred_program(self) -> String {
        self.command_override()
            .or_else(|| self.discovered_program_candidates().into_iter().next())
            .unwrap_or_else(|| self.default_program_name().to_string())
    }

    pub(crate) fn discovered_program_candidates(self) -> Vec<String> {
        let mut programs = Vec::new();
        if let Some(program) = self.command_override() {
            push_unique_string(&mut programs, program);
            return programs;
        }

        for program in find_programs_on_path(env::var_os("PATH").as_deref(), self.path_names()) {
            push_unique_string(&mut programs, program);
        }

        for path in self.known_program_paths() {
            push_unique_path_if_present(&mut programs, path);
        }

        if programs.is_empty() {
            push_unique_string(&mut programs, self.default_program_name().to_string());
        }

        programs
    }

    pub(crate) fn discovered_pip_candidates(self) -> Vec<String> {
        let mut programs = Vec::new();
        if let Some(program) = self.command_override() {
            push_unique_string(&mut programs, program);
            return programs;
        }

        for program in find_programs_on_path(env::var_os("PATH").as_deref(), pip_path_names()) {
            push_unique_string(&mut programs, program);
        }

        for path in pip_known_program_paths() {
            push_unique_path_if_present(&mut programs, path);
        }

        if programs.is_empty() {
            push_unique_string(&mut programs, "python".to_string());
            push_unique_string(&mut programs, "python3".to_string());
            push_unique_string(&mut programs, "pip".to_string());
        }

        programs
    }

    pub(crate) fn default_program_name(self) -> &'static str {
        match self {
            Self::Winget => "winget",
            Self::Scoop => "scoop",
            Self::Chocolatey => "choco",
            Self::Npm => {
                if cfg!(windows) {
                    "npm.cmd"
                } else {
                    "npm"
                }
            }
            Self::Pip => "python",
        }
    }

    pub(crate) fn path_names(self) -> &'static [&'static str] {
        match self {
            Self::Winget => {
                if cfg!(windows) {
                    &["winget.exe"]
                } else {
                    &["winget"]
                }
            }
            Self::Scoop => {
                if cfg!(windows) {
                    &["scoop.cmd", "scoop.exe", "scoop.ps1"]
                } else {
                    &["scoop"]
                }
            }
            Self::Chocolatey => {
                if cfg!(windows) {
                    &["choco.exe", "choco.bat"]
                } else {
                    &["choco"]
                }
            }
            Self::Npm => {
                if cfg!(windows) {
                    &["npm.cmd", "npm.exe"]
                } else {
                    &["npm"]
                }
            }
            Self::Pip => &[],
        }
    }

    pub(crate) fn version_probe_args(self) -> Vec<String> {
        match self {
            Self::Winget | Self::Scoop | Self::Npm => vec!["--version".to_string()],
            Self::Chocolatey => vec!["-v".to_string()],
            Self::Pip => pip_probe_args("python"),
        }
    }

    pub(crate) fn known_program_paths(self) -> Vec<PathBuf> {
        match self {
            Self::Winget => winget_known_program_paths(),
            Self::Scoop => scoop_known_program_paths(),
            Self::Chocolatey => choco_known_program_paths(),
            Self::Npm => npm_known_program_paths(),
            Self::Pip => pip_known_program_paths(),
        }
    }
}

/// Where the `--deep` index cache lives.
///
/// Under LOCALAPPDATA rather than next to config.toml: the cache is ~45 MB of
/// regenerable data, while the config dir holds a small file people may edit
/// or version-control.
fn pip_index_cache_path() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(env::temp_dir);
    base.join(APP_NAME).join("pypi-index.json")
}

