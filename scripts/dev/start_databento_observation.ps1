param(
    [string]$ConfigPath = "config/runtime.local.toml",
    [string]$StrategyPath = "strategies/examples/gc_momentum_fade_v1.md",
    [switch]$StartDashboard
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$resolvedConfigPath = (Resolve-Path (Join-Path $repoRoot $ConfigPath)).Path
$resolvedStrategyPath = (Resolve-Path (Join-Path $repoRoot $StrategyPath)).Path
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

if (-not $env:TV_BOT__MARKET_DATA__API_KEY) {
    throw @"
TV_BOT__MARKET_DATA__API_KEY is not set.

Set it in this PowerShell session first, for example:
  `$env:TV_BOT__MARKET_DATA__API_KEY = "db-..."

Then rerun:
  .\scripts\dev\start_databento_observation.ps1
"@
}

New-Item -ItemType Directory -Force -Path $logsDir | Out-Null

if (-not (Test-Path $runtimeExe) -or -not (Test-Path $cliExe)) {
    Push-Location $repoRoot
    try {
        cargo build --release -p tv-bot-runtime -p tv-bot-cli
    } finally {
        Pop-Location
    }
}

Stop-ExistingProcess -ProcessName "tv-bot-runtime.exe" -CommandLineFragment $resolvedConfigPath
Stop-ExistingProcess -ProcessName "node.exe" -CommandLineFragment "vite"

Remove-Item $runtimeOutLog, $runtimeErrLog, $dashboardOutLog, $dashboardErrLog -ErrorAction SilentlyContinue

Start-Process `
    -FilePath "powershell" `
    -ArgumentList "-NoLogo", "-NoProfile", "-Command", "Set-Location '$repoRoot'; & '$runtimeExe' '$resolvedConfigPath'" `
    -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $runtimeOutLog `
    -RedirectStandardError $runtimeErrLog | Out-Null

Wait-ForUrl -Url $healthUrl

& $cliExe --config $resolvedConfigPath load $resolvedStrategyPath | Out-Host
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
