//! Configuration file loading, saving, and backend status reporting.
//!
//! The config is a small TOML subset parsed by hand (see `parse_bool` /
//! `render_toml_string`), because the crate has no dependencies.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) backend: Option<Backend>,
    pub(crate) assume_yes: bool,
    pub(crate) auto_elevate: bool,
    pub(crate) winget_source: Option<String>,
    pub(crate) choco_source: Option<String>,
    /// Buckets the user wants searched/pinned, in priority order.
    ///
    /// Replaces the old single `scoop_bucket`. A Scoop install normally spans
    /// main, extras and sometimes versions, so one scalar could not describe it.
    pub(crate) scoop_buckets: Vec<String>,
    /// Bucket to prefer when a package name is not qualified with `bucket/pkg`.
    /// `None` means "let Scoop resolve it", which is the correct default:
    /// forcing a prefix made `waw install git` ask for `extras/git`.
    pub(crate) scoop_default_bucket: Option<String>,
    /// Deprecated single-bucket key, still parsed for existing configs.
    pub(crate) scoop_bucket: Option<String>,
    pub(crate) pip_user: bool,
    pub(crate) command_timeout_seconds: u64,
    pub(crate) enable_winget: bool,
    pub(crate) enable_scoop: bool,
    pub(crate) enable_choco: bool,
    pub(crate) enable_npm: bool,
    pub(crate) enable_pip: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: None,
            assume_yes: true,
            auto_elevate: true,
            winget_source: None,
            choco_source: None,
            scoop_buckets: Vec::new(),
            scoop_default_bucket: None,
            scoop_bucket: None,
            pip_user: true,
            command_timeout_seconds: DEFAULT_COMMAND_TIMEOUT_SECONDS,
            enable_winget: true,
            enable_scoop: true,
            enable_choco: true,
            enable_npm: true,
            enable_pip: true,
        }
    }
}

impl Config {
    pub(crate) fn load_from(path: &Path, explicit: bool) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == ErrorKind::NotFound && !explicit => Ok(Self::default()),
            Err(error) => Err(format!("failed to read config {}: {error}", path.display())),
        }
    }

    pub(crate) fn parse(contents: &str) -> Result<Self, String> {
        let mut config = Self::default();

        for (line_number, raw_line) in contents.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!(
                    "invalid config line {}: expected key = value",
                    line_number + 1
                )
            })?;
            let key = key.trim();
            let value = value.trim();

            match key {
                "backend" => config.backend = Some(Backend::parse(&parse_string(value)?)?),
                "assume_yes" => config.assume_yes = parse_bool(value)?,
                "auto_elevate" => config.auto_elevate = parse_bool(value)?,
                "winget_source" => config.winget_source = Some(parse_string(value)?),
                "choco_source" => config.choco_source = Some(parse_string(value)?),
                "scoop_buckets" => config.scoop_buckets = parse_string_list(value)?,
                "scoop_default_bucket" => {
                    config.scoop_default_bucket = Some(parse_string(value)?)
                }
                // Deprecated: a single bucket used to mean "prefix everything
                // with it". Treat it as a default instead, which is what users
                // actually wanted; also record it in the list.
                "scoop_bucket" => {
                    let bucket = parse_string(value)?;
                    if !config.scoop_buckets.iter().any(|item| item == &bucket) {
                        config.scoop_buckets.push(bucket.clone());
                    }
                    config.scoop_bucket = Some(bucket.clone());
                    if config.scoop_default_bucket.is_none() {
                        config.scoop_default_bucket = Some(bucket);
                    }
                }
                "pip_user" => config.pip_user = parse_bool(value)?,
            "command_timeout_seconds" => {
                config.command_timeout_seconds = value.parse::<u64>().map_err(|_| {
                    format!("invalid value for command_timeout_seconds: {value}")
                })?;
            }
                "enable_winget" => config.enable_winget = parse_bool(value)?,
                "enable_scoop" => config.enable_scoop = parse_bool(value)?,
                "enable_choco" => config.enable_choco = parse_bool(value)?,
                "enable_npm" => config.enable_npm = parse_bool(value)?,
                "enable_pip" => config.enable_pip = parse_bool(value)?,
                _ => {
                    return Err(format!(
                        "unsupported config key on line {}: {key}",
                        line_number + 1
                    ));
                }
            }
        }

        Ok(config)
    }

    pub(crate) fn winget_source(&self) -> Option<&str> {
        self.winget_source
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }

    pub(crate) fn choco_source(&self) -> Option<&str> {
        self.choco_source
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }

    /// Prefixes a Scoop package with the default bucket, but only when that
    /// bucket is explicitly configured and is not `main`.
    ///
    /// `main` is Scoop's implicit default, so prefixing it changes nothing
    /// except to break `pkg@version` specs. Anything already qualified
    /// (`extras/foo`) is left alone.
    pub(crate) fn qualify_scoop_package(&self, package: &str) -> String {
        if package.contains('/') {
            return package.to_string();
        }

        let bucket = self
            .scoop_default_bucket
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| !value.eq_ignore_ascii_case("main"));

        match bucket {
            Some(bucket) => {
                // Preserve a `pkg@version` suffix: `gh@2.7.0` must become
                // `extras/gh@2.7.0`, not `extras/gh@2.7.0` mis-split.
                match package.split_once('@') {
                    Some((name, version)) => format!("{bucket}/{name}@{version}"),
                    None => format!("{bucket}/{package}"),
                }
            }
            None => package.to_string(),
        }
    }

    pub(crate) fn is_backend_enabled(&self, backend: Backend) -> bool {
        match backend {
            Backend::Winget => self.enable_winget,
            Backend::Scoop => self.enable_scoop,
            Backend::Chocolatey => self.enable_choco,
            Backend::Npm => self.enable_npm,
            Backend::Pip => self.enable_pip,
        }
    }

    pub(crate) fn set_backend_enabled(&mut self, backend: Backend, enabled: bool) {
        match backend {
            Backend::Winget => self.enable_winget = enabled,
            Backend::Scoop => self.enable_scoop = enabled,
            Backend::Chocolatey => self.enable_choco = enabled,
            Backend::Npm => self.enable_npm = enabled,
            Backend::Pip => self.enable_pip = enabled,
        }

        if !enabled && self.backend == Some(backend) {
            self.backend = None;
        }
    }

    pub(crate) fn to_toml(&self) -> String {
        let mut lines = Vec::new();
        if let Some(backend) = self.backend {
            lines.push(format!("backend = \"{backend}\""));
        }
        lines.push(format!("assume_yes = {}", self.assume_yes));
        lines.push(format!("auto_elevate = {}", self.auto_elevate));
        if let Some(source) = &self.winget_source {
            lines.push(format!("winget_source = {}", render_toml_string(source)));
        }
        if let Some(source) = &self.choco_source {
            lines.push(format!("choco_source = {}", render_toml_string(source)));
        }
        if !self.scoop_buckets.is_empty() {
            let rendered = self
                .scoop_buckets
                .iter()
                .map(|bucket| render_toml_string(bucket))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("scoop_buckets = [{rendered}]"));
        }
        if let Some(bucket) = &self.scoop_default_bucket {
            lines.push(format!(
                "scoop_default_bucket = {}",
                render_toml_string(bucket)
            ));
        }
        lines.push(format!("pip_user = {}", self.pip_user));
        lines.push(format!(
            "command_timeout_seconds = {}",
            self.command_timeout_seconds
        ));
        lines.push(format!("enable_winget = {}", self.enable_winget));
        lines.push(format!("enable_scoop = {}", self.enable_scoop));
        lines.push(format!("enable_choco = {}", self.enable_choco));
        lines.push(format!("enable_npm = {}", self.enable_npm));
        lines.push(format!("enable_pip = {}", self.enable_pip));
        lines.join("\n") + "\n"
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedConfig {
    pub(crate) path: Option<PathBuf>,
    pub(crate) config: Config,
}

impl LoadedConfig {
    pub(crate) fn load(explicit_path: Option<&Path>) -> Result<Self, String> {
        let path = explicit_path
            .map(Path::to_path_buf)
            .or_else(default_config_path);
        let config = match explicit_path {
            Some(path) => Config::load_from(path, true)?,
            None => match path.as_deref() {
                Some(path) => Config::load_from(path, false)?,
                None => Config::default(),
            },
        };
        Ok(Self { path, config })
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Err("could not resolve a config path for saving".to_string());
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create config directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::write(path, self.config.to_toml())
            .map_err(|error| format!("failed to write config {}: {error}", path.display()))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BackendStatus {
    pub(crate) backend: Backend,
    pub(crate) supported: bool,
    pub(crate) enabled: bool,
    pub(crate) available: bool,
    pub(crate) default_selected: bool,
}

pub(crate) fn collect_backend_statuses(config: &Config) -> Vec<BackendStatus> {
    [
        Backend::Winget,
        Backend::Scoop,
        Backend::Chocolatey,
        Backend::Npm,
        Backend::Pip,
    ]
    .into_iter()
    .map(|backend| BackendStatus {
        backend,
        supported: backend.supported_on_host(),
        enabled: config.is_backend_enabled(backend),
        available: backend.is_available(),
        default_selected: config.backend == Some(backend),
    })
    .collect()
}

pub(crate) fn print_backend_statuses(config: &Config) {
    for status in collect_backend_statuses(config) {
        print_backend_status_line(&status);
    }
}

pub(crate) fn print_backend_status(backend: Backend, config: &Config) {
    if let Some(status) = collect_backend_statuses(config)
        .into_iter()
        .find(|status| status.backend == backend)
    {
        print_backend_status_line(&status);
    }
}

pub(crate) fn print_backend_status_line(status: &BackendStatus) {
    let support = if status.supported {
        "supported"
    } else {
        "unsupported"
    };
    let enabled = if status.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let availability = if status.available {
        "available"
    } else {
        "missing"
    };
    let default_mark = if status.default_selected {
        " default"
    } else {
        ""
    };

    println!(
        "{:>10}: {support}, {enabled}, {availability}{default_mark}",
        status.backend
    );

    if !status.available {
        println!(
            "            install hint: {}",
            status.backend.install_hint()
        );
    }
}

pub(crate) fn render_backend_statuses_json(config: &Config) -> String {
    let items = collect_backend_statuses(config)
        .into_iter()
        .map(|status| render_backend_status_object(&status))
        .collect::<Vec<_>>()
        .join(",\n  ");
    format!("[\n  {items}\n]")
}

pub(crate) fn render_backend_status_json(backend: Backend, config: &Config) -> String {
    collect_backend_statuses(config)
        .into_iter()
        .find(|status| status.backend == backend)
        .map(|status| render_backend_status_object(&status))
        .unwrap_or_else(|| "{}".to_string())
}

pub(crate) fn render_backend_status_object(status: &BackendStatus) -> String {
    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"supported\":{},",
            "\"enabled\":{},",
            "\"available\":{},",
            "\"default_selected\":{},",
            "\"install_hint\":\"{}\"",
            "}}"
        ),
        status.backend,
        status.supported,
        status.enabled,
        status.available,
        status.default_selected,
        escape_json_string(status.backend.install_hint()),
    )
}

pub(crate) fn render_backend_install_json(
    backend: Backend,
    available_before: bool,
    enable_requested: bool,
    dry_run: bool,
    result: &str,
    invocation: Option<&Invocation>,
) -> String {
    let command = invocation
        .map(Invocation::render_for_display)
        .unwrap_or_default();
    format!(
        concat!(
            "{{",
            "\"backend\":\"{}\",",
            "\"result\":\"{}\",",
            "\"available_before\":{},",
            "\"enable_requested\":{},",
            "\"dry_run\":{},",
            "\"bootstrap_command\":\"{}\",",
            "\"install_hint\":\"{}\"",
            "}}"
        ),
        backend,
        escape_json_string(result),
        available_before,
        enable_requested,
        dry_run,
        escape_json_string(&command),
        escape_json_string(backend.install_hint()),
    )
}
