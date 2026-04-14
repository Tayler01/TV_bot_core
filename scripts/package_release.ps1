param(
    [string]$OutputRoot = "dist/releases"
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$outputDir = Join-Path $repoRoot $OutputRoot
$cargoToml = Join-Path $repoRoot "Cargo.toml"
$dashboardDir = Join-Path $repoRoot "apps/dashboard"
$runtimeBinary = Join-Path $repoRoot "target/release/tv-bot-runtime.exe"
$cliBinary = Join-Path $repoRoot "target/release/tv-bot-cli.exe"

function Get-WorkspaceVersion {
    param([string]$ManifestPath)

    $manifestContent = Get-Content $ManifestPath -Raw
    $match = [regex]::Match($manifestContent, '(?m)^\s*version\s*=\s*"([^"]+)"\s*$')
    if (-not $match.Success) {
        throw "Unable to determine workspace version from $ManifestPath"
    }

    return $match.Groups[1].Value
}

Push-Location $repoRoot
try {
    $version = Get-WorkspaceVersion -ManifestPath $cargoToml
    $commit = (git rev-parse --short HEAD).Trim()
    $builtAtUtc = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $bundleName = "tv-bot-core-$version-windows-x86_64-$commit"
    $bundleDir = Join-Path $outputDir $bundleName
    $archivePath = Join-Path $outputDir "$bundleName.zip"

    if (Test-Path $bundleDir) {
        Remove-Item -LiteralPath $bundleDir -Recurse -Force
    }
    if (Test-Path $archivePath) {
        Remove-Item -LiteralPath $archivePath -Force
    }

    New-Item -ItemType Directory -Force -Path $bundleDir | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir "bin") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir "dashboard") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir "config") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir "docs/ops") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $bundleDir "strategies/examples") | Out-Null

    cargo build --release -p tv-bot-runtime -p tv-bot-cli

    Push-Location $dashboardDir
    try {
        npm ci
        npm run build
    }
    finally {
        Pop-Location
    }

    Copy-Item -LiteralPath $runtimeBinary -Destination (Join-Path $bundleDir "bin/tv-bot-runtime.exe")
    Copy-Item -LiteralPath $cliBinary -Destination (Join-Path $bundleDir "bin/tv-bot-cli.exe")
    Copy-Item -Path (Join-Path $repoRoot "apps/dashboard/dist/*") -Destination (Join-Path $bundleDir "dashboard") -Recurse
    Copy-Item -LiteralPath (Join-Path $repoRoot "config/runtime.example.toml") -Destination (Join-Path $bundleDir "config/runtime.example.toml")
    Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination (Join-Path $bundleDir "README.md")
    Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination (Join-Path $bundleDir "LICENSE")
    Copy-Item -LiteralPath (Join-Path $repoRoot "STRATEGY_SPEC.md") -Destination (Join-Path $bundleDir "STRATEGY_SPEC.md")
    Copy-Item -Path (Join-Path $repoRoot "docs/ops/*") -Destination (Join-Path $bundleDir "docs/ops") -Recurse
    Copy-Item -Path (Join-Path $repoRoot "strategies/examples/*") -Destination (Join-Path $bundleDir "strategies/examples") -Recurse

    $manifest = [ordered]@{
        package_name = "tv-bot-core"
        version = $version
        git_commit = $commit
        built_at_utc = $builtAtUtc
        platform = "windows-x86_64"
        contents = [ordered]@{
            runtime_binary = "bin/tv-bot-runtime.exe"
            cli_binary = "bin/tv-bot-cli.exe"
            dashboard = "dashboard"
            runtime_config = "config/runtime.example.toml"
            ops_docs = "docs/ops"
            strategy_examples = "strategies/examples"
        }
    } | ConvertTo-Json -Depth 4

    Set-Content -LiteralPath (Join-Path $bundleDir "release-manifest.json") -Value $manifest -Encoding utf8

    Compress-Archive -Path $bundleDir -DestinationPath $archivePath
    Write-Host "Created release bundle at $archivePath"
}
finally {
    Pop-Location
}
