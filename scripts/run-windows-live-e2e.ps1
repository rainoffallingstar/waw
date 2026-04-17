param(
    [string]$Binary = ""
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

function Resolve-WawBinary {
    param(
        [string]$Requested
    )

    if ($Requested -ne "") {
        return (Resolve-Path $Requested).Path
    }

    $releaseBinary = Join-Path $projectRoot "target\release\waw.exe"
    if (Test-Path $releaseBinary) {
        return $releaseBinary
    }

    $debugBinary = Join-Path $projectRoot "target\debug\waw.exe"
    if (Test-Path $debugBinary) {
        return $debugBinary
    }

    throw "Could not find waw.exe. Build the project first or pass -Binary."
}

function Invoke-Waw {
    param(
        [string[]]$Arguments,
        [switch]$AllowFailure
    )

    $fullArgs = @("--config", $script:ConfigPath, "--no-elevate") + $Arguments
    Write-Host "waw $($fullArgs -join ' ')" -ForegroundColor Cyan
    $stdoutPath = Join-Path $tempRoot ([guid]::NewGuid().ToString() + ".stdout.log")
    $stderrPath = Join-Path $tempRoot ([guid]::NewGuid().ToString() + ".stderr.log")

    try {
        $process = Start-Process `
            -FilePath $script:WawBinary `
            -ArgumentList $fullArgs `
            -Wait `
            -PassThru `
            -NoNewWindow `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath

        $stdout = if (Test-Path -LiteralPath $stdoutPath) {
            Get-Content -LiteralPath $stdoutPath -Raw
        } else {
            ""
        }
        $stderr = if (Test-Path -LiteralPath $stderrPath) {
            Get-Content -LiteralPath $stderrPath -Raw
        } else {
            ""
        }
        $output = ($stdout + $stderr)
        $exitCode = $process.ExitCode
    }
    finally {
        Remove-Item -LiteralPath $stdoutPath -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stderrPath -ErrorAction SilentlyContinue
    }

    if (-not $AllowFailure -and $exitCode -ne 0) {
        throw "waw failed with exit code ${exitCode}:`n$output"
    }

    return [pscustomobject]@{
        ExitCode = $exitCode
        Output = $output
    }
}

function Assert-OutputContains {
    param(
        [string]$Output,
        [string]$Pattern,
        [string]$Message
    )

    if ($Output -notmatch $Pattern) {
        throw "$Message`nPattern: $Pattern`nOutput:`n$Output"
    }
}

function Assert-OutputNotContains {
    param(
        [string]$Output,
        [string]$Pattern,
        [string]$Message
    )

    if ($Output -match $Pattern) {
        throw "$Message`nPattern: $Pattern`nOutput:`n$Output"
    }
}

function Get-BackendStatuses {
    return (Invoke-Waw @("backends")).Output
}

function Ensure-BackendAvailable {
    param(
        [string]$Backend
    )

    $statuses = Get-BackendStatuses
    $backendStatusPattern = "(?m)^\s*" + [regex]::Escape($Backend) + ":\s+supported,\s+enabled,\s+available(?:\s+default)?\s*$"

    if ($statuses -match $backendStatusPattern) {
        Write-Host "Backend $Backend is available." -ForegroundColor Green
        return
    }

    $dryRun = Invoke-Waw @("--dry-run", "backend", "install", $Backend, "--enable")
    $backendNamePattern = [regex]::Escape($Backend)
    Assert-OutputContains $dryRun.Output $backendNamePattern "Dry-run bootstrap for $Backend did not produce output."

    if ($Backend -eq "winget") {
        throw "winget is required for the live E2E workflow but is not available on this runner."
    }

    Invoke-Waw @("backend", "install", $Backend, "--enable")
    $statuses = Get-BackendStatuses
    if ($statuses -notmatch $backendStatusPattern) {
        throw "Backend $Backend is still unavailable after bootstrap request.`n$statuses"
    }
}

function Get-BackendListOutput {
    param(
        [string]$Backend
    )

    return (Invoke-Waw @("--backend", $Backend, "list")).Output
}

function Ensure-PackageAbsent {
    param(
        [hashtable]$Case
    )

    if ($Case.SkipListVerification) {
        return
    }

    $listOutput = Get-BackendListOutput $Case.Backend
    if ($listOutput -match $Case.VerifyPattern) {
        Invoke-Waw @("--backend", $Case.Backend, "remove", "--exact", $Case.Package)
        $listOutput = Get-BackendListOutput $Case.Backend
    }

    Assert-OutputNotContains $listOutput $Case.VerifyPattern "Package $($Case.Package) should not be installed for backend $($Case.Backend) before the test."
}

function Invoke-InstallRemoveCase {
    param(
        [hashtable]$Case
    )

    Invoke-Step "$($Case.Backend) install/remove for $($Case.Package)" {
        Ensure-BackendAvailable $Case.Backend
        Ensure-PackageAbsent $Case

        Invoke-Waw @("--backend", $Case.Backend, "install", "--exact", $Case.Package)
        if (-not $Case.SkipListVerification) {
            $listAfterInstall = Get-BackendListOutput $Case.Backend
            Assert-OutputContains $listAfterInstall $Case.VerifyPattern "Package $($Case.Package) was not detected after install on backend $($Case.Backend)."
        }

        Invoke-Waw @("--backend", $Case.Backend, "remove", "--exact", $Case.Package)
        if (-not $Case.SkipListVerification) {
            $listAfterRemove = Get-BackendListOutput $Case.Backend
            Assert-OutputNotContains $listAfterRemove $Case.VerifyPattern "Package $($Case.Package) is still present after remove on backend $($Case.Backend)."
        }
    }
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $projectRoot

$script:WawBinary = Resolve-WawBinary $Binary
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "waw-live-e2e"
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null
$script:ConfigPath = Join-Path $tempRoot "config.toml"

@"
assume_yes = true
auto_elevate = false
winget_source = "winget"
enable_winget = true
enable_scoop = true
enable_choco = true
enable_npm = true
enable_pip = true
pip_user = true
"@ | Set-Content -LiteralPath $script:ConfigPath -Encoding ASCII

Invoke-Step "Printing backend status snapshot" {
    Get-BackendStatuses | Write-Host
}

Invoke-Step "Checking bootstrap command coverage" {
    foreach ($backend in @("scoop", "choco", "npm", "pip")) {
        Invoke-Waw @("--dry-run", "backend", "install", $backend, "--enable") | Out-Null
    }
}

$cases = @(
    @{
        Backend = "winget"
        Package = "jqlang.jq"
        VerifyPattern = "(?mi)^\s*winget\s+(jq|jqlang\.jq)\s+"
        SkipListVerification = $true
    },
    @{
        Backend = "scoop"
        Package = "jq"
        VerifyPattern = "(?mi)^\s*scoop\s+jq\s+"
    },
    @{
        Backend = "choco"
        Package = "jq"
        VerifyPattern = "(?mi)^\s*choco\s+jq\s+"
    },
    @{
        Backend = "npm"
        Package = "cowsay"
        VerifyPattern = "(?mi)^\s*npm\s+cowsay\s+"
    },
    @{
        Backend = "pip"
        Package = "pyfiglet"
        VerifyPattern = "(?mi)^\s*pip\s+pyfiglet\s+"
    }
)

foreach ($case in $cases) {
    Invoke-InstallRemoveCase $case
}

Write-Host "Live Windows end-to-end checks completed successfully." -ForegroundColor Green
