//! waw-owned hold list for backends that have no native pin.
//!
//! winget, scoop and choco all ship a real "do not upgrade this" switch, so
//! waw delegates to it and records the same name here only so `hold --list`
//! can show one complete table.
//!
//! pip and npm have nothing equivalent. There, this file *is* the hold: the
//! upgrade path consults it and skips what it names, which makes
//! `waw hold --backend pip requests` mean the same thing as a winget pin.
//!
//! The file lives next to `config.toml` and is a flat TOML subset:
//!
//!     pip = ["requests", "django"]
//!     npm = ["typescript"]
//!
//! Anything unparsable is an error rather than a silent reset -- losing a hold
//! would silently re-enable upgrades the user asked to stop.

use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Holds {
    /// Kept as an ordered list so the file and its display stay stable.
    entries: Vec<(Backend, Vec<String>)>,
}

impl Holds {
    pub(crate) fn path_for(config_path: Option<&Path>) -> Option<PathBuf> {
        match config_path {
            Some(path) => path.parent().map(|dir| dir.join("holds.toml")),
            None => default_config_path().map(|path| path.with_file_name("holds.toml")),
        }
    }

    pub(crate) fn load(config_path: Option<&Path>) -> Result<Self, String> {
        let path = match Self::path_for(config_path) {
            Some(path) => path,
            None => return Ok(Self::default()),
        };

        match fs::read_to_string(&path) {
            Ok(contents) => Self::parse(&contents)
                .map_err(|error| format!("failed to parse holds {}: {error}", path.display())),
            // No file yet is the normal first-run state, not an error.
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("failed to read holds {}: {error}", path.display())),
        }
    }

    pub(crate) fn parse(contents: &str) -> Result<Self, String> {
        let mut holds = Self::default();

        for (line_number, raw_line) in contents.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!(
                    "invalid holds line {}: expected key = value",
                    line_number + 1
                )
            })?;
            let backend = Backend::parse(key.trim())
                .map_err(|error| format!("invalid holds line {}: {error}", line_number + 1))?;
            let packages = parse_string_list(value)
                .map_err(|error| format!("invalid holds line {}: {error}", line_number + 1))?;
            for package in packages {
                holds.insert(backend, package);
            }
        }

        Ok(holds)
    }

    pub(crate) fn to_toml(&self) -> String {
        let mut lines = vec![
            "# waw-owned holds, used for backends without a native pin.".to_string(),
            "# Edit with `waw hold` / `waw hold --off` so native pins stay in sync.".to_string(),
        ];
        for (backend, packages) in &self.entries {
            if packages.is_empty() {
                continue;
            }
            let rendered = packages
                .iter()
                .map(|package| render_toml_string(package))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("{backend} = [{rendered}]"));
        }
        lines.join("\n") + "\n"
    }

    pub(crate) fn insert(&mut self, backend: Backend, package: String) {
        if let Some((_, packages)) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| *candidate == backend)
        {
            if !packages
                .iter()
                .any(|existing| same_package(existing, &package))
            {
                packages.push(package);
                packages.sort();
            }
            return;
        }

        self.entries.push((backend, vec![package]));
        // Keep the file and `--list` in a stable, backend-ordered shape. Done
        // as a full sort rather than an insert so an index can never be
        // computed past the end of the vector.
        self.entries
            .sort_by_key(|(candidate, _)| backend_order(*candidate));
    }

    pub(crate) fn remove(&mut self, backend: Backend, package: &str) -> bool {
        let Some((_, packages)) = self
            .entries
            .iter_mut()
            .find(|(candidate, _)| *candidate == backend)
        else {
            return false;
        };

        let before = packages.len();
        packages.retain(|existing| !same_package(existing, package));
        before != packages.len()
    }

    pub(crate) fn contains(&self, backend: Backend, package: &str) -> bool {
        self.entries
            .iter()
            .find(|(candidate, _)| *candidate == backend)
            .map(|(_, packages)| {
                packages
                    .iter()
                    .any(|existing| same_package(existing, package))
            })
            .unwrap_or(false)
    }

    pub(crate) fn packages(&self, backend: Backend) -> &[String] {
        self.entries
            .iter()
            .find(|(candidate, _)| *candidate == backend)
            .map(|(_, packages)| packages.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn save(&self, config_path: Option<&Path>) -> Result<(), String> {
        let path = Self::path_for(config_path)
            .ok_or_else(|| "could not resolve a path for holds.toml".to_string())?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create holds directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::write(&path, self.to_toml())
            .map_err(|error| format!("failed to write holds {}: {error}", path.display()))
    }

    /// Drops held packages from an `upgrade` target list.
    ///
    /// Returns the kept packages plus the ones that were skipped, so the
    /// caller can say what it did instead of silently upgrading less.
    pub(crate) fn filter_upgrade_targets(
        &self,
        backend: Backend,
        packages: &[String],
    ) -> (Vec<String>, Vec<String>) {
        // Only backends without a native pin need this filter; the others
        // already refuse at the tool level, and re-checking here would make a
        // stale holds.toml override a pin the user removed on purpose.
        if !backend.uses_waw_holds() {
            return (packages.to_vec(), Vec::new());
        }

        let mut kept = Vec::new();
        let mut skipped = Vec::new();
        for package in packages {
            // Strip any version selector. `requests==2.31` (pip) and
            // `typescript@5` (npm) name the same package as a bare name, so a
            // hold must cover all three spellings.
            let name = PackageSpec::parse(package).name;
            if self.contains(backend, &name) {
                skipped.push(package.clone());
            } else {
                kept.push(package.clone());
            }
        }
        (kept, skipped)
    }
}

fn same_package(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn backend_order(backend: Backend) -> usize {
    Backend::ALL
        .iter()
        .position(|candidate| *candidate == backend)
        .unwrap_or(Backend::ALL.len())
}
