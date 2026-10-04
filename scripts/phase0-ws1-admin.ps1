# Phase 0, WS1 one-time admin setup:
#   1.1a  move the page file from C: to F: (4-32 GB; see set-pagefile.ps1 for why); takes effect after reboot
#   1.13  install smartmontools, take a first SMART snapshot, schedule monthly + on-disk-error snapshots
# Run elevated. Log: F:\Nebula\setup\ws1-admin.log
$ErrorActionPreference = 'Stop'

$setupDir = 'F:\Nebula\setup'
New-Item -ItemType Directory -Force -Path $setupDir, 'F:\Nebula\state\smart' | Out-Null
Start-Transcript -Path (Join-Path $setupDir 'ws1-admin.log') -Append | Out-Null

try {
    Write-Host '== 1.1a Page file =='
    $cs = Get-CimInstance Win32_ComputerSystem
    if ($cs.AutomaticManagedPagefile) {
        Set-CimInstance -InputObject $cs -Property @{ AutomaticManagedPagefile = $false }
    }
    Get-CimInstance Win32_PageFileSetting |
        Where-Object { $_.Name -notlike 'F:*' } |
        Remove-CimInstance

    $f = Get-CimInstance Win32_PageFileSetting | Where-Object { $_.Name -like 'F:*' }
    if (-not $f) {
        $f = New-CimInstance -ClassName Win32_PageFileSetting -Property @{ Name = 'F:\pagefile.sys' }
    }
    Set-CimInstance -InputObject $f -Property @{ InitialSize = [uint32]4096; MaximumSize = [uint32]32768 }
    Get-CimInstance Win32_PageFileSetting | Format-Table Name, InitialSize, MaximumSize -AutoSize | Out-String | Write-Host

    Write-Host '== 1.13 smartmontools =='
    $smartctl = Join-Path $env:ProgramFiles 'smartmontools\bin\smartctl.exe'
    if (-not (Test-Path $smartctl)) {
        winget install --id smartmontools.smartmontools -e --silent `
            --accept-source-agreements --accept-package-agreements
    }

    $snapshot = Join-Path $setupDir 'smart-snapshot.ps1'
    Copy-Item -Force (Join-Path $PSScriptRoot 'smart-snapshot.ps1') $snapshot
    & $snapshot
    Get-ChildItem 'F:\Nebula\state\smart' | Format-Table Name, Length -AutoSize | Out-String | Write-Host

    $action = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$snapshot`""
    schtasks /Create /F /TN 'Nebula\SMART monthly' /RU SYSTEM /RL HIGHEST /SC MONTHLY /D 1 /ST 04:00 /TR $action
    $diskErrors = "*[System[Provider[@Name='disk'] and (EventID=7 or EventID=51 or EventID=153)]]"
    schtasks /Create /F /TN 'Nebula\SMART on disk error' /RU SYSTEM /RL HIGHEST /SC ONEVENT /EC System /MO $diskErrors /TR $action

    Write-Host 'DONE. Reboot to apply the page file change.'
}
catch {
    Write-Host "FAILED: $_"
    throw
}
finally {
    Stop-Transcript | Out-Null
}
