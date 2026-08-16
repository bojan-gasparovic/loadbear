# Produce the Windows installer.
#
# This exists because the helper is a sidecar, and a sidecar has to be built and
# renamed before the bundler runs. `cargo tauri build` will not do it: it builds
# the interface only, and if the sidecar is absent or stale it either fails or,
# worse, bundles the last one that happened to be lying there.
#
# The renaming is Tauri's convention, not ours. `externalBin` looks for the
# binary suffixed with the target triple and installs it without the suffix, so
# `loadbear-service-x86_64-pc-windows-msvc.exe` lands beside the interface as
# `loadbear-service.exe`, which is the name `service_control::helper_path`
# already looks for.
#
# Output: target\release\bundle\nsis\LoadBear_<version>_x64-setup.exe

# Deliberately not "Stop". Windows PowerShell 5.1 wraps a native program's
# stderr in an ErrorRecord, and cargo reports its ordinary progress there, so
# "Stop" aborts on a successful build. Exit codes are checked by hand instead,
# and the cmdlets that can genuinely fail carry -ErrorAction Stop themselves.
$ErrorActionPreference = "Continue"
$root = $PSScriptRoot

$triple = ((rustc -vV | Select-String '^host: ') -split ' ')[1]
Write-Host "Target: $triple"

Write-Host "Building the helper..."
cargo build --release -p loadbear-service
if ($LASTEXITCODE -ne 0) { throw "the helper did not build" }

$binaries = Join-Path $root "crates\loadbear-app\binaries"
New-Item -ItemType Directory -Force $binaries -ErrorAction Stop | Out-Null
Copy-Item (Join-Path $root "target\release\loadbear-service.exe") `
          (Join-Path $binaries "loadbear-service-$triple.exe") -Force -ErrorAction Stop
Write-Host "Sidecar staged."

Write-Host "Bundling..."
Push-Location (Join-Path $root "crates\loadbear-app")
try {
    cargo tauri build
    if ($LASTEXITCODE -ne 0) { throw "the bundle did not build" }
} finally {
    Pop-Location
}

$setup = Get-ChildItem (Join-Path $root "target\release\bundle\nsis") -Filter "*-setup.exe" |
         Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host ""
Write-Host "Installer: $($setup.FullName)"
Write-Host "Size: $([math]::Round($setup.Length / 1MB, 1)) MB"
