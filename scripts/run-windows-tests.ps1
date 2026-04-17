param(
    [switch]$Clippy
)

$ErrorActionPreference = "Stop"

function Invoke-Step {
    param(
        [string]$Label,
        [scriptblock]$Action
    )

    Write-Host "==> $Label"
    & $Action
}

function Get-LatestBinary {
    param(
        [string]$Pattern
    )

    $item = Get-ChildItem -Path "target/debug/deps" -Filter $Pattern |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if (-not $item) {
        throw "Could not find test binary matching pattern: $Pattern"
    }

    return $item.FullName
}

Invoke-Step "Building tests" {
    cargo test --no-run
}

if ($Clippy) {
    Invoke-Step "Running clippy" {
        cargo clippy --all-targets --all-features -- -D warnings
    }
}

$unitTestBinary = Get-LatestBinary "waw-*.exe"
$integrationBinary = Get-LatestBinary "e2e-*.exe"

Invoke-Step "Running unit tests via $unitTestBinary" {
    & $unitTestBinary
}

Invoke-Step "Running integration tests via $integrationBinary" {
    & $integrationBinary
}
