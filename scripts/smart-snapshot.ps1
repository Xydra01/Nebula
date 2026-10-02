# Writes one SMART JSON report per drive to F:\Nebula\state\smart\ (Phase 0 task 1.13).
# Must run elevated; scheduled monthly and on disk error events.
$ErrorActionPreference = 'Stop'

$outDir = 'F:\Nebula\state\smart'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$smartctl = Join-Path $env:ProgramFiles 'smartmontools\bin\smartctl.exe'
if (-not (Test-Path $smartctl)) { throw "smartctl not found at $smartctl" }

$stamp = Get-Date -Format 'yyyy-MM-dd_HHmm'
$scan = & $smartctl --scan -j | ConvertFrom-Json

# Intel RST exposes the same drives again as csmi devices; keep one report per serial.
$seen = @{}
foreach ($dev in $scan.devices) {
    $report = & $smartctl -a -j -d $dev.type $dev.name | Out-String
    $serial = ($report | ConvertFrom-Json).serial_number
    if ($serial -and $seen.ContainsKey($serial)) { continue }
    if ($serial) { $seen[$serial] = $true }
    $safeName = ($dev.name -replace '[^A-Za-z0-9]', '_').Trim('_')
    $outFile = Join-Path $outDir "$safeName-$stamp.json"
    $report | Out-File -FilePath $outFile -Encoding utf8
}
