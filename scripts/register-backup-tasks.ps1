# Registers the scheduled backups (PHASE0_PLAN 7.1): every 6 hours (00/06/12/18, skipped when
# nothing changed) and nightly at 03:00 (always uploads). They run whether or not you're logged
# in, which needs your Windows password; Task Scheduler stores it, this script does not.
# Run from an elevated PowerShell. Re-running replaces the tasks.
$ErrorActionPreference = 'Stop'

$nebula = 'F:\Nebula\bin\nebula.exe'
if (-not (Test-Path $nebula)) { throw "$nebula not found; run scripts\install-nebula.ps1 first" }
if (-not (Get-Command rclone -ErrorAction SilentlyContinue)) {
    throw 'rclone is not on PATH; the tasks would fail to upload'
}

$user = "$env:USERDOMAIN\$env:USERNAME"
$cred = Get-Credential -UserName $user -Message 'Windows password, so backups run while logged out'
$plain = $cred.GetNetworkCredential().Password

$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -DontStopIfGoingOnBatteries `
    -AllowStartIfOnBatteries -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Hours 1)

$sixHourly = 0, 6, 12, 18 | ForEach-Object { New-ScheduledTaskTrigger -Daily -At ([datetime]::Today.AddHours($_)) }
$tasks = @(
    @{ Name = 'Nebula backup (6-hourly)'; Args = 'backup now --if-changed'; Triggers = $sixHourly },
    @{ Name = 'Nebula backup (nightly)'; Args = 'backup now'; Triggers = @(New-ScheduledTaskTrigger -Daily -At 3am) }
)
try {
    foreach ($t in $tasks) {
        $action = New-ScheduledTaskAction -Execute $nebula -Argument $t.Args -WorkingDirectory 'F:\Nebula\bin'
        Register-ScheduledTask -TaskName $t.Name -TaskPath '\Nebula\' -Action $action -Trigger $t.Triggers `
            -Settings $settings -User $user -Password $plain -RunLevel Limited -Force | Out-Null
        Write-Host "Registered \Nebula\$($t.Name)"
    }
} finally {
    $plain = $null
}

Write-Host 'Test run of the 6-hourly task...'
Start-ScheduledTask -TaskPath '\Nebula\' -TaskName 'Nebula backup (6-hourly)'
Start-Sleep -Seconds 20
$info = Get-ScheduledTaskInfo -TaskPath '\Nebula\' -TaskName 'Nebula backup (6-hourly)'
Write-Host "Last result: $($info.LastTaskResult) (0 = success); run 'nebula doctor' to see the backup check."
