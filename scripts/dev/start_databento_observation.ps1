param(
    [string]$ConfigPath = "config/runtime.local.toml",
    [string]$StrategyPath,
    [switch]$StartDashboard
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$resolvedConfigPath = (Resolve-Path (Join-Path $repoRoot $ConfigPath)).Path
$runtimeExe = Join-Path $repoRoot "target/release/tv-bot-runtime.exe"
$cliExe = Join-Path $repoRoot "target/release/tv-bot-cli.exe"
$logsDir = Join-Path $repoRoot "logs"
$runtimeOutLog = Join-Path $logsDir "runtime-host.out.log"
$runtimeErrLog = Join-Path $logsDir "runtime-host.err.log"
$dashboardOutLog = Join-Path $logsDir "dashboard.out.log"
$dashboardErrLog = Join-Path $logsDir "dashboard.err.log"
$healthUrl = "http://127.0.0.1:8080/health"
$statusUrl = "http://127.0.0.1:8080/status"
$readinessUrl = "http://127.0.0.1:8080/readiness"
$dashboardUrl = "http://127.0.0.1:4173"

function Resolve-StrategyPathFromConfig {
    param(
        [string]$ConfigFilePath
    )

    $match = Select-String -Path $ConfigFilePath -Pattern '^\s*default_strategy_path\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) {
        return $null
    }

    $rawPath = $match.Matches[0].Groups[1].Value
    if ([string]::IsNullOrWhiteSpace($rawPath)) {
        return $null
    }

    return (Resolve-Path (Join-Path $repoRoot $rawPath)).Path
}

function Stop-ExistingProcess {
    param(
        [string]$ProcessName,
        [string]$CommandLineFragment
    )

    $existing = Get-CimInstance Win32_Process |
        Where-Object {
            $_.Name -eq $ProcessName -and
            $_.CommandLine -and
            $_.CommandLine -like "*$CommandLineFragment*"
        }

    foreach ($process in $existing) {
        Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue
    }
}

function Wait-ForUrl {
    param(
        [string]$Url,
        [int]$Attempts = 40,
        [int]$DelayMilliseconds = 500
    )

    for ($attempt = 0; $attempt -lt $Attempts; $attempt++) {
        try {
            $response = Invoke-WebRequest -UseBasicParsing $Url -TimeoutSec 5
            if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 500) {
                return
            }
        } catch {
        }

        Start-Sleep -Milliseconds $DelayMilliseconds
    }

    throw "Timed out waiting for $Url"
}

function Get-EffectiveDatabentoApiKey {
    $processDatabentoKey = $env:DATABENTO_API_KEY
    $processLegacyKey = $env:TV_BOT__MARKET_DATA__API_KEY

    if (-not $processDatabentoKey) {
        $processDatabentoKey = [Environment]::GetEnvironmentVariable("DATABENTO_API_KEY", "User")
    }

    if (-not $processLegacyKey) {
        $processLegacyKey = [Environment]::GetEnvironmentVariable("TV_BOT__MARKET_DATA__API_KEY", "User")
    }

    if ($processDatabentoKey) {
        return @{
            Key = $processDatabentoKey
            Source = if ($env:DATABENTO_API_KEY) { "process:DATABENTO_API_KEY" } else { "user:DATABENTO_API_KEY" }
        }
    }

    if ($processLegacyKey) {
        return @{
            Key = $processLegacyKey
            Source = if ($env:TV_BOT__MARKET_DATA__API_KEY) { "process:TV_BOT__MARKET_DATA__API_KEY" } else { "user:TV_BOT__MARKET_DATA__API_KEY" }
        }
    }

    return $null
}

$effectiveDatabento = Get-EffectiveDatabentoApiKey

if ($effectiveDatabento) {
    $env:DATABENTO_API_KEY = $effectiveDatabento.Key
    $env:TV_BOT__MARKET_DATA__API_KEY = $effectiveDatabento.Key
}

if (-not $env:TV_BOT__MARKET_DATA__API_KEY) {
    throw @"
No Databento API key is set.

Set it in this PowerShell session first, or persist it at the Windows user level, for example:
  `$env:DATABENTO_API_KEY = "db-..."

or:
  `$env:TV_BOT__MARKET_DATA__API_KEY = "db-..."

Persistent Windows user env example:
  [Environment]::SetEnvironmentVariable("DATABENTO_API_KEY", "db-...", "User")

Then rerun:
  .\scripts\dev\start_databento_observation.ps1
"@
}

$resolvedStrategyPath = $null
if ($StrategyPath) {
    $resolvedStrategyPath = (Resolve-Path (Join-Path $repoRoot $StrategyPath)).Path
} else {
    $resolvedStrategyPath = Resolve-StrategyPathFromConfig -ConfigFilePath $resolvedConfigPath
}

New-Item -ItemType Directory -Force -Path $logsDir | Out-Null

Stop-ExistingProcess -ProcessName "tv-bot-runtime.exe" -CommandLineFragment $resolvedConfigPath
Stop-ExistingProcess -ProcessName "node.exe" -CommandLineFragment "vite"

# Always rebuild before launch so the observation stack cannot accidentally
# run stale local binaries after dashboard or host control-plane changes.
Push-Location $repoRoot
try {
    cargo build --release -p tv-bot-runtime -p tv-bot-cli
} finally {
    Pop-Location
}

Remove-Item $runtimeOutLog, $runtimeErrLog, $dashboardOutLog, $dashboardErrLog -ErrorAction SilentlyContinue

Start-Process `
    -FilePath "powershell" `
    -ArgumentList "-NoLogo", "-NoProfile", "-Command", "Set-Location '$repoRoot'; & '$runtimeExe' '$resolvedConfigPath'" `
    -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $runtimeOutLog `
    -RedirectStandardError $runtimeErrLog | Out-Null

Wait-ForUrl -Url $healthUrl

if ($resolvedStrategyPath) {
    & $cliExe --config $resolvedConfigPath load $resolvedStrategyPath | Out-Host
}
& $cliExe --config $resolvedConfigPath warmup start | Out-Host

if ($StartDashboard) {
    Start-Process `
        -FilePath "powershell" `
        -ArgumentList "-NoLogo", "-NoProfile", "-Command", "Set-Location '$repoRoot\apps\dashboard'; npm run dev" `
        -WorkingDirectory (Join-Path $repoRoot "apps/dashboard") `
        -RedirectStandardOutput $dashboardOutLog `
        -RedirectStandardError $dashboardErrLog | Out-Null
}

Write-Host ""
Write-Host "Databento observation stack is ready."
Write-Host "Databento key source: $($effectiveDatabento.Source)"
Write-Host "Runtime root:     http://127.0.0.1:8080/"
Write-Host "Runtime status:   $statusUrl"
Write-Host "Runtime readiness:$readinessUrl"
Write-Host "Dashboard:        $dashboardUrl"
Write-Host ""
Write-Host "Next checks:"
Write-Host "  $cliExe --config $resolvedConfigPath status"
Write-Host "  $cliExe --config $resolvedConfigPath readiness"
Write-Host ""
Write-Host "Logs:"
Write-Host "  $runtimeOutLog"
Write-Host "  $runtimeErrLog"
