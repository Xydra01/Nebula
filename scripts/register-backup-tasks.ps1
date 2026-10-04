# Registers the scheduled backups (PHASE0_PLAN 7.1): every 6 hours (00/06/12/18, skipped when
# nothing changed) and nightly at 03:00 (always uploads). Run from an elevated PowerShell.
# Re-running replaces the tasks.
#
# By default the tasks run whether or not you're logged in, which needs your Windows password
# (for a Microsoft account, the account password, not the PIN). Task Scheduler stores it; this
# script does not. With -WhenLoggedOn no password is needed, but runs only happen while you're
# logged in; missed ones start at your next logon.
param([switch]$WhenLoggedOn)
$ErrorActionPreference = 'Stop'

$nebula = 'F:\Nebula\bin\nebula.exe'
if (-not (Test-Path $nebula)) { throw "$nebula not found; run scripts\install-nebula.ps1 first" }
if (-not (Get-Command rclone -ErrorAction SilentlyContinue)) {
    throw 'rclone is not on PATH; the tasks would fail to upload'
}

$user = "$env:USERDOMAIN\$env:USERNAME"
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -DontStopIfGoingOnBatteries `
    -AllowStartIfOnBatteries -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Hours 1) -Hidden

$sixHourly = 0, 6, 12, 18 | ForEach-Object { New-ScheduledTaskTrigger -Daily -At ([datetime]::Today.AddHours($_)) }
$tasks = @(
    @{ Name = 'Nebula backup (6-hourly)'; Args = 'backup now --if-changed'; Triggers = $sixHourly },
    @{ Name = 'Nebula backup (nightly)'; Args = 'backup now'; Triggers = @(New-ScheduledTaskTrigger -Daily -At 3am) }
)

if ($WhenLoggedOn) {
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    $plain = $null
} else {
    $cred = Get-Credential -UserName $user -Message 'Windows password (Microsoft account password, not the PIN)'
    $plain = $cred.GetNetworkCredential().Password
}

function New-BackupAction([string]$arguments) {
    if ($WhenLoggedOn) {
        # Runs in your desktop session; headless conhost keeps a console window from flashing up.
        New-ScheduledTaskAction -Execute 'conhost.exe' -Argument "--headless `"$nebula`" $arguments" -WorkingDirectory 'F:\Nebula\bin'
    } else {
        New-ScheduledTaskAction -Execute $nebula -Argument $arguments -WorkingDirectory 'F:\Nebula\bin'
    }
}

try {
    foreach ($t in $tasks) {
        $common = @{
            TaskName    = $t.Name
            TaskPath    = '\Nebula\'
            Action      = New-BackupAction $t.Args
            Trigger     = $t.Triggers
            Settings    = $settings
            Force       = $true
            ErrorAction = 'Stop'
        }
        if ($WhenLoggedOn) {
            Register-ScheduledTask @common -Principal $principal | Out-Null
        } else {
            Register-ScheduledTask @common -User $user -Password $plain -RunLevel Limited | Out-Null
        }
        Write-Host "Registered \Nebula\$($t.Name)"
    }
} catch {
    if ($_.Exception.HResult -eq -2147023570) {
        throw 'Windows rejected the password. For a Microsoft account use the account password, not the PIN; or re-run with -WhenLoggedOn.'
    }
    throw
} finally {
    $plain = $null
}

Write-Host 'Test run of the 6-hourly task...'
Start-ScheduledTask -TaskPath '\Nebula\' -TaskName 'Nebula backup (6-hourly)'
do {
    Start-Sleep -Seconds 5
    $task = Get-ScheduledTask -TaskPath '\Nebula\' -TaskName 'Nebula backup (6-hourly)'
} while ($task.State -eq 'Running')
$info = Get-ScheduledTaskInfo -TaskPath '\Nebula\' -TaskName 'Nebula backup (6-hourly)'
Write-Host "Last result: $($info.LastTaskResult) (0 = success); run 'nebula doctor' to see the backup check."
