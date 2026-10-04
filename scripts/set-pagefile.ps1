# Sets the size of F:\pagefile.sys (the only page file). Run elevated; reboot to apply.
# The maximum is about commit charge, not paging: on Windows (WDDM) llama-server commits roughly as
# much system memory as the VRAM it uses (~10.7 GB for `standard`), on top of the desktop's ~33 GB.
# With a 12 GB maximum the limit was ~44 GB and allocations inside llama-server failed at random.
# Log: F:\Nebula\setup\pagefile.log
param(
    [uint32] $InitialMB = 4096,
    [uint32] $MaximumMB = 32768
)
$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from an elevated PowerShell.'
}

New-Item -ItemType Directory -Force -Path 'F:\Nebula\setup' | Out-Null
Start-Transcript -Path 'F:\Nebula\setup\pagefile.log' -Append | Out-Null
try {
    $cs = Get-CimInstance Win32_ComputerSystem
    if ($cs.AutomaticManagedPagefile) {
        Set-CimInstance -InputObject $cs -Property @{ AutomaticManagedPagefile = $false }
    }
    $f = Get-CimInstance Win32_PageFileSetting | Where-Object { $_.Name -like 'F:*' }
    if (-not $f) {
        $f = New-CimInstance -ClassName Win32_PageFileSetting -Property @{ Name = 'F:\pagefile.sys' }
    }
    Write-Host ('Before: {0} initial {1} MB, max {2} MB' -f $f.Name, $f.InitialSize, $f.MaximumSize)
    Set-CimInstance -InputObject $f -Property @{ InitialSize = $InitialMB; MaximumSize = $MaximumMB }
    Get-CimInstance Win32_PageFileSetting | Format-Table Name, InitialSize, MaximumSize -AutoSize | Out-String | Write-Host
    Write-Host 'DONE. Reboot to apply. Check afterwards: (Get-CimInstance Win32_OperatingSystem).TotalVirtualMemorySize / 1MB is about 64 (GB).'
}
finally {
    Stop-Transcript | Out-Null
}
