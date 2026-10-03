# (c) 2026 erik - https://erik.xyz - erik@erik.xyz
# Package smoke (Windows): unzip the portable zip -> assert layout/manifest -> start
# xd-daemon.exe with --image -> pipe one ping -> assert pong.
# Deliberately does NOT use --listen/--port-file/--owner-pid: the elevated-session chain is
# covered by the T1/T4 tests; the layout assertion here (engine next to the app executable)
# is what keeps the UI's daemon auto-discovery -- and with it the elevation flow -- alive.
# ASCII-only on purpose, see package-windows.ps1.
[CmdletBinding()]
param([string]$Zip)

$ErrorActionPreference = 'Stop'
# The daemon writes its banner and diagnostics to stderr by design; keep that from turning
# into a terminating error (pwsh 7.3+ honors this preference when set to $true).
$PSNativeCommandUseErrorActionPreference = $false
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not $Zip) {
  $Zip = Get-ChildItem -Path dist -Filter 'xiaodun-v*-windows-x64.zip' -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $Zip -or -not (Test-Path $Zip)) {
  throw 'FAIL: no zip found (run scripts/package-windows.ps1 first)'
}
Write-Host "smoke: $Zip"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('xd-smoke-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
try {
  Expand-Archive -Path $Zip -DestinationPath $tmp
  $pkg = Get-ChildItem -Path $tmp -Directory | Select-Object -First 1
  if (-not $pkg) { throw 'FAIL: zip has no top-level directory' }

  # Manifest + layout: app and engine are resolved from the SAME directory (packaging
  # convention the UI relies on); data/ and README-*.txt come from the Flutter bundle.
  $app = Join-Path $pkg.FullName 'xiaodun.exe'
  $daemon = Join-Path $pkg.FullName 'xd-daemon.exe'
  $required = @(
    $app,
    $daemon,
    (Join-Path $pkg.FullName 'data'),
    (Join-Path $pkg.FullName 'flutter_windows.dll')
  )
  foreach ($f in $required) {
    if (-not (Test-Path $f)) { throw "FAIL: missing from package: $f" }
  }
  if (-not (Get-ChildItem (Join-Path $pkg.FullName 'README-*.txt') -ErrorAction SilentlyContinue)) {
    throw 'FAIL: missing README-*.txt from package'
  }

  $img = Join-Path $tmp 'test.img'
  [IO.File]::WriteAllBytes($img, [byte[]]::new(65536))

  $out = '{"jsonrpc":"2.0","id":1,"method":"ping","params":null}' | & $daemon --image $img
  $out | Out-Host
  if (-not ($out -match '"pong":true')) { throw 'FAIL: ping produced no pong' }

  Write-Host 'PACKAGE SMOKE OK (Windows)'
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
