param(
    [string]$Binary = "",
    # Mutating commands need admin. On an interactive machine waw can raise a
    # UAC prompt, but a CI runner has nobody to click it, so those cases are
    # skipped unless this switch is passed.
    [switch]$AllowElevation
)

$ErrorActionPreference = "Stop"

$script:StepTimeoutMs = 300000

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

function Quote-Arg {
    param([string]$Value)
    if ($Value -match '^[\w.:/\\@=+-]+$') { return $Value }
    return '"' + ($Value -replace '"', '\"') + '"'
}

function Invoke-Waw {
    param(
        [string[]]$Arguments,
        [switch]$AllowFailure
    )

    # Mutating commands must be allowed to elevate: Chocolatey refuses to run
    # unattended without admin and blocks on a "Do you want to continue?"
    # prompt that no CI runner can answer. Read-only commands stay
    # non-elevated so they never trigger a UAC dialog.
    $mutating = ($Arguments | Where-Object { $_ -in @("install", "remove", "upgrade", "update") }) -ne $null
    $elevationArg = if ($mutating) { "--elevate" } else { "--no-elevate" }
    $fullArgs = @("--config", $script:ConfigPath, $elevationArg) + $Arguments
    Write-Host "waw $($fullArgs -join ' ')" -ForegroundColor Cyan
    $stdoutPath = Join-Path $tempRoot ([guid]::NewGuid().ToString() + ".stdout.log")
    $stderrPath = Join-Path $tempRoot ([guid]::NewGuid().ToString() + ".stderr.log")

    try {
        # Start-Process -PassThru does not reliably surface ExitCode when the
        # process object is created with redirection, which made every command
        # look like a -1 failure. A plain ProcessStartInfo returns the real
        # value.
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $script:WawBinary
        $psi.Arguments = ($fullArgs | ForEach-Object { Quote-Arg $_ }) -join ' '
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.CreateNoWindow = $true

        $process = [System.Diagnostics.Process]::Start($psi)
        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        if (-not $process.WaitForExit($script:StepTimeoutMs)) {
            try { $process.Kill($true) } catch { }
            throw "waw command timed out after $([int]($script:StepTimeoutMs / 1000))s: waw $($fullArgs -join ' ')"
        }
        $process.WaitForExit()

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

# The runner does not ship every package manager (Scoop in particular is a
# per-user install). A backend that is simply absent is not a regression --
# skip it and say so, rather than failing the whole gate.
$available = @{}
$statusText = (Invoke-Waw @("backends")).Output
foreach ($line in ($statusText -split "`n")) {
    if ($line -match '^(?<name>\w+):.*available$') {
        $available[$Matches['name']] = $true
    }
}
if ($available.Count -eq 0) {
    throw "No backend is available on this runner; live E2E cannot verify anything."
}

$ran = 0
$skippedElevation = 0
foreach ($case in $cases) {
    if (-not $available.ContainsKey($case.Backend)) {
        Write-Host "skipping $($case.Backend): not installed on this runner" -ForegroundColor Yellow
        continue
    }
    # Only Chocolatey requires administrator rights. winget, scoop, npm and
    # pip all install per-user, so their cases can run without UAC.
    if ($case.Backend -eq "choco" -and -not $AllowElevation) {
        Write-Host "skipping choco: needs elevation (-AllowElevation)" -ForegroundColor Yellow
        $skippedElevation += 1
        continue
    }
    Invoke-InstallRemoveCase $case
    $ran += 1
}
if ($ran -eq 0) {
    if ($skippedElevation -gt 0) {
        Write-Host "Only non-mutating checks ran (elevation disabled)." -ForegroundColor Yellow
        return
    }
    throw "No case could run: every configured backend was unavailable."
}

# A green gate must not be read as "every mutating path was exercised".
# Say plainly what was and was not covered.
$unverified = @($cases | Where-Object { -not $available.ContainsKey($_.Backend) } | ForEach-Object { $_.Backend })
if ($skippedElevation -gt 0) { $unverified += "choco" }
if ($unverified.Count -gt 0) {
    Write-Host ""
    Write-Host "UNVERIFIED in this run: $($unverified -join ', ')" -ForegroundColor Yellow
    Write-Host "  (unavailable on this host, or needs an interactive UAC prompt)"
    Write-Host "  These paths are covered by local runs; CI cannot exercise them."
}
Write-Host "Ran $ran of $($cases.Count) install/remove cases."

Write-Host "Live Windows end-to-end checks completed successfully." -ForegroundColor Green
