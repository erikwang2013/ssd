# (c) 2026 erik - https://erik.xyz - erik@erik.xyz
# Package the Windows portable zip: Flutter release bundle + xd-daemon.exe + README-*.txt.
# Requires Windows + Flutter (3.47.5, CI-pinned) + Rust stable. Invoked by the manual CI
# job `package-windows` (workflow_dispatch input packaging=true, see README).
# Unverified locally: the dev machine is Linux; behavior is evidenced by a CI dispatch run.
# ASCII-only on purpose: PowerShell 5.1 reads BOM-less scripts as ANSI, so a non-ASCII
# literal (the README file name, paths) would break there. Keep this file ASCII.
[CmdletBinding()]
param([string]$OutDir = 'dist')

$ErrorActionPreference = 'Stop'
# flutter/cargo write progress and diagnostics to stderr by design; keep that from turning
# into a terminating error (pwsh 7.3+ honors this preference when set to $true).
$PSNativeCommandUseErrorActionPreference = $false
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' |
  Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'FAIL: cannot read version from workspace Cargo.toml' }
Write-Host "packaging xiaodun v$version"

Push-Location ui
try {
  flutter build windows --release
  if ($LASTEXITCODE -ne 0) { throw "FAIL: flutter build windows --release ($LASTEXITCODE)" }
} finally { Pop-Location }

cargo build --locked --release -p xd-daemon
if ($LASTEXITCODE -ne 0) { throw "FAIL: cargo build -p xd-daemon ($LASTEXITCODE)" }

$release = Join-Path $root 'ui/build/windows/x64/runner/Release'
if (-not (Test-Path $release)) { throw "FAIL: Flutter bundle not found: $release" }

# Zip root = top-level folder named after the archive (both platforms use this shape).
$name = "xiaodun-v$version-windows-x64"
$stage = Join-Path $root (Join-Path $OutDir $name)
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item -Recurse -Force (Join-Path $release '*') $stage
# Flutter's runner is BINARY_NAME=xiaodun_ui; ship it under the product name.
Rename-Item (Join-Path $stage 'xiaodun_ui.exe') 'xiaodun.exe'
# Engine goes NEXT TO the app executable: the UI resolves a sibling xd-daemon.exe
# (ui/lib/core_client/ipc_transport.dart, packagedDaemonPath). Do not move it elsewhere.
Copy-Item (Join-Path $root 'target/release/xd-daemon.exe') $stage
Copy-Item (Join-Path $root 'packaging/windows/README-*.txt') $stage

$zip = Join-Path $root (Join-Path $OutDir "$name.zip")
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "built: $zip"
