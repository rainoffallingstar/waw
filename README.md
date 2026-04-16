# waw

`waw` is a Rust command-line package manager frontend for Windows-style workflows.
It exposes an `apt-get`-like interface while delegating work to:

- `winget`
- `scoop`
- `choco`
- `npm`
- `pip`

It was originally prototyped from the Rust version built during the UniGetUI-inspired work, and is now structured as its own standalone repository.

## Commands

- `update`
- `upgrade`
- `install`
- `install --pick`
- `remove`
- `hold`
- `search`
- `list`
- `show`
- `backends`
- `backend list`
- `backend enable`
- `backend disable`
- `backend install`
- `backend default`

## Usage

```text
waw [--backend <winget|scoop|choco|npm|pip>] [--config <path>] [--dry-run] [--json] [-y] <command> [args...]
```

Examples:

```powershell
waw update
waw install Git.Git
waw install --pick git
waw search requests
waw list --upgradable
waw show pip
waw --json show pip
waw backends
waw backend enable pip
waw backend default auto
```

## Features

- Auto-detects enabled and available backends.
- Supports interactive `yay`-style selection with `install --pick`.
- Aggregates `search` across multiple enabled backends in auto mode.
- Normalizes `list` output into a combined table when possible.
- Normalizes `show` output into structured package details.
- Renders multi-backend `show` results as a comparison view.
- Supports machine-readable JSON output for backend management and `show`.

## Config

Default config paths:

- Windows: `%APPDATA%\waw\config.toml`
- Linux/macOS: `$XDG_CONFIG_HOME/waw/config.toml`
- Fallback: `$HOME/.config/waw/config.toml`

Supported keys:

```toml
backend = "winget"
assume_yes = true
winget_source = "winget"
choco_source = "https://community.chocolatey.org/api/v2/"
scoop_bucket = "extras"
pip_user = true
enable_winget = true
enable_scoop = true
enable_choco = true
enable_npm = true
enable_pip = true
```

## Build

Debug build:

```powershell
cargo build
```

Release build:

```powershell
cargo build --release
```

Windows release helper:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows-release.ps1
```

## JSON Show Output

`waw --json show <package>` emits an array of per-backend results. Each item includes:

- `backend`
- `command`
- `success`
- `dry_run`
- `details`
- `raw_output`
- `error`

The `details` object includes normalized fields such as:

- `name`
- `version`
- `summary`
- `homepage`
- `license`
- `author`
- `repository`
- `keywords`
- `dependencies`
- `extra_fields`
