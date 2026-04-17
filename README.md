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
- `install --exact`
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
waw [--backend <winget|scoop|choco|npm|pip>] [--config <path>] [--dry-run] [--json] [--interactive] [--no-elevate] <command> [args...]
```

Examples:

```powershell
waw update
waw --interactive upgrade
waw upgrade
waw install git
waw install --exact Git.Git
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
- Runs `update` and full-system `upgrade` across all enabled and available backends in auto mode.
- Discovers backend commands from environment overrides, `PATH`, and common Windows install locations.
- Defaults to non-interactive execution and can auto-request Windows elevation for mutating commands.
- Defers `install <query>` elevation until after package selection, then performs a single elevated install pass when needed.
- Uses interactive `yay`-style selection as the default `install` behavior.
- Preserves direct package installation via `install --exact`.
- Aggregates `search` across multiple enabled backends in auto mode.
- Normalizes `list` output into a combined table when possible.
- Normalizes `show` output into structured package details.
- Renders multi-backend `show` results as a comparison view.
- Supports machine-readable JSON output for backend management and `show`.

`backend install <name>` is a bootstrap helper. It is currently supported only where this project has a host-specific bootstrap path, which today means Windows-oriented flows for `scoop`, `choco`, `npm`, and `pip`.

## Config

Default config paths:

- Windows: `%APPDATA%\waw\config.toml`
- Linux/macOS: `$XDG_CONFIG_HOME/waw/config.toml`
- Fallback: `$HOME/.config/waw/config.toml`

Supported keys:

```toml
backend = "winget"
assume_yes = true
auto_elevate = true
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

## Backend Command Overrides

For testing, CI, or custom installations, you can override the executable used for each backend:

```powershell
$env:WAW_WINGET_CMD = "C:\tools\winget.exe"
$env:WAW_SCOOP_CMD = "C:\Users\me\scoop\shims\scoop.cmd"
$env:WAW_CHOCO_CMD = "C:\ProgramData\chocolatey\bin\choco.exe"
$env:WAW_NPM_CMD = "C:\Program Files\nodejs\npm.cmd"
$env:WAW_PYTHON_CMD = "C:\Python313\python.exe"
```

`WAW_PYTHON_CMD` should point to a Python executable that can run `-m pip`.

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

Windows test helper:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\run-windows-tests.ps1
```

Include `-Clippy` to run `cargo clippy --all-targets --all-features -- -D warnings` before executing the test binaries.

Live Windows end-to-end test:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\run-windows-live-e2e.ps1 -Binary .\target\release\waw.exe
```

## CI

The repository includes a Windows CI workflow that runs:

- `powershell -ExecutionPolicy Bypass -File .\scripts\run-windows-tests.ps1`
- `cargo build --release`
- `powershell -ExecutionPolicy Bypass -File .\scripts\run-windows-live-e2e.ps1 -Binary .\target\release\waw.exe`
- `powershell -ExecutionPolicy Bypass -File .\scripts\build-windows-release.ps1`

On pushes to the default branch and on manual workflow runs, a rolling prerelease is published after the test gates pass. The release tag format is `YYYY-MM-DD` in the Asia/Shanghai calendar, and every successful build on the same day reuses that same tag and overwrites the `waw.exe` asset.

## Beta Smoke Checklist

1. Run `cargo run -- upgrade` in a real Windows administrator environment.
2. Confirm only one UAC prompt appears.
3. Confirm the current terminal continues showing output after elevation.
4. Run `cargo run -- install git` and confirm it searches first, then elevates only at the install stage.
5. Run `cargo run -- backends` and confirm it does not report an unusable `winget` alias as available.

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
