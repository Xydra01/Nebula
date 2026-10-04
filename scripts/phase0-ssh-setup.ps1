# Phase 0, WS1 task 1.10: Windows OpenSSH Server, reachable only over Tailscale, key login only.
#   - installs the OpenSSH.Server capability and starts sshd automatically
#   - installs the given public key for an administrator account (administrators_authorized_keys)
#   - disables password and keyboard-interactive login
#   - restricts the inbound port 22 rule to the Tailscale ranges
# Run elevated. Log: F:\Nebula\setup\ssh-setup.log
param(
    [string] $PublicKeyFile = 'F:\Nebula\setup\macbook.pub',
    [switch] $PowerShellShell
)
$ErrorActionPreference = 'Stop'

$setupDir = 'F:\Nebula\setup'
Start-Transcript -Path (Join-Path $setupDir 'ssh-setup.log') -Append | Out-Null

try {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this script from an elevated PowerShell.'
    }
    $key = (Get-Content -Raw $PublicKeyFile).Trim()
    if ($key -notmatch '^(ssh-ed25519|ssh-rsa|ecdsa-sha2-\S+) \S+') { throw "$PublicKeyFile is not an SSH public key" }

    Write-Host '== OpenSSH Server =='
    $cap = Get-WindowsCapability -Online -Name 'OpenSSH.Server*'
    if ($cap.State -ne 'Installed') {
        Add-WindowsCapability -Online -Name $cap.Name | Out-Null
    }
    # The first start generates the host keys and the default sshd_config.
    Set-Service sshd -StartupType Automatic
    Start-Service sshd

    $sshDir = Join-Path $env:ProgramData 'ssh'
    $config = Join-Path $sshDir 'sshd_config'

    Write-Host '== Authorized key =='
    # For members of Administrators, sshd reads only this file, never ~/.ssh/authorized_keys.
    $authKeys = Join-Path $sshDir 'administrators_authorized_keys'
    $existing = if (Test-Path $authKeys) { Get-Content $authKeys } else { @() }
    if ($existing -notcontains $key) {
        Add-Content -Path $authKeys -Value $key -Encoding ascii
    }
    # sshd ignores the file unless only Administrators and SYSTEM can access it (SIDs: locale-proof).
    icacls $authKeys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F' | Out-Null

    Write-Host '== sshd_config =='
    Copy-Item $config "$config.bak-$(Get-Date -Format yyyyMMdd-HHmmss)"
    $managed = 'PubkeyAuthentication', 'PasswordAuthentication', 'KbdInteractiveAuthentication',
        'PermitEmptyPasswords'
    $lines = Get-Content $config | Where-Object {
        $l = $_.Trim()
        -not ($managed | Where-Object { $l -match "^$_\s" }) -and $l -notmatch '^# Nebula'
    }
    # sshd uses the first value it sees, and settings must precede any Match block, so they go first.
    $header = @(
        '# Nebula (PHASE0_PLAN 1.10): key-only login.',
        'PubkeyAuthentication yes',
        'PasswordAuthentication no',
        'KbdInteractiveAuthentication no',
        'PermitEmptyPasswords no'
    )
    Set-Content -Path $config -Value ($header + $lines) -Encoding ascii
    & (Join-Path $env:WINDIR 'System32\OpenSSH\sshd.exe') -t
    if ($LASTEXITCODE -ne 0) { throw 'sshd -t rejected the new sshd_config (backup kept next to it)' }

    if ($PowerShellShell) {
        $ps = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
        New-Item -Path 'HKLM:\SOFTWARE\OpenSSH' -Force | Out-Null
        New-ItemProperty -Path 'HKLM:\SOFTWARE\OpenSSH' -Name DefaultShell -Value $ps -PropertyType String -Force | Out-Null
    }

    Write-Host '== Firewall =='
    # 100.64.0.0/10 is Tailscale's IPv4 range; fd7a:115c:a1e0::/48 its IPv6 range.
    $tailnet = @('100.64.0.0/10', 'fd7a:115c:a1e0::/48')
    $rule = Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue
    if (-not $rule) {
        $rule = New-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -DisplayName 'OpenSSH Server (sshd)' `
            -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow -Program "$env:WINDIR\System32\OpenSSH\sshd.exe"
    }
    $rule | Set-NetFirewallRule -Enabled True -Profile Any -RemoteAddress $tailnet
    # Any other inbound rule for port 22 would bypass the restriction.
    Get-NetFirewallRule -Direction Inbound -Enabled True -Action Allow |
        Where-Object { $_.Name -ne 'OpenSSH-Server-In-TCP' } |
        Where-Object { ($_ | Get-NetFirewallPortFilter).LocalPort -contains '22' } |
        ForEach-Object { Write-Host "disabling other port-22 rule: $($_.DisplayName)"; $_ | Disable-NetFirewallRule }

    Restart-Service sshd

    Write-Host '== Result =='
    Get-Service sshd | Format-List Name, Status, StartType | Out-String | Write-Host
    $rule = Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP'
    Write-Host "firewall: enabled=$($rule.Enabled) remote=$((($rule | Get-NetFirewallAddressFilter).RemoteAddress) -join ', ')"
    Write-Host "keys in administrators_authorized_keys: $((Get-Content $authKeys).Count)"
    Write-Host 'Done. From the Mac: ssh myfri@ej-pc'
} finally {
    Stop-Transcript | Out-Null
}
