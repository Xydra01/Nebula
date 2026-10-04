# Builds nebula.exe and nebula-daemon.exe (release) and copies them to F:\Nebula\bin\.
# Stop the daemon first (`nebula daemon stop`); a running nebula-daemon.exe can't be replaced.
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$bin = 'F:\Nebula\bin'
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'F:\Nebula\build\target' }

$running = Get-Process -Name 'nebula-daemon' -ErrorAction SilentlyContinue
if ($running) { throw "nebula-daemon is running (pid $($running.Id -join ', ')); run 'nebula daemon stop' first" }

Push-Location $repo
try {
    cargo build --release --locked -p nebula-cli -p nebula-daemon
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $bin | Out-Null
foreach ($exe in 'nebula.exe', 'nebula-daemon.exe') {
    Copy-Item -Force (Join-Path $env:CARGO_TARGET_DIR "release\$exe") (Join-Path $bin $exe)
}
Write-Host "Installed to $bin"
& (Join-Path $bin 'nebula.exe') --version
