param(
    [string]$Target = "",
    [string]$OutDir = "dist"
)

$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $projectRoot

$cargoArgs = @("build", "--release")
if ($Target -ne "") {
    $cargoArgs += @("--target", $Target)
}

Write-Host "Running: cargo $($cargoArgs -join ' ')" -ForegroundColor Cyan
cargo @cargoArgs

$targetRoot = if ($Target -ne "") {
    Join-Path $projectRoot "target\$Target\release"
} else {
    Join-Path $projectRoot "target\release"
}

$exePath = Join-Path $targetRoot "waw.exe"
if (-not (Test-Path $exePath)) {
    throw "Build succeeded but $exePath was not found."
}

$distDir = Join-Path $projectRoot $OutDir
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Copy-Item $exePath (Join-Path $distDir "waw.exe") -Force

Write-Host "Release artifact copied to $distDir\waw.exe" -ForegroundColor Green
