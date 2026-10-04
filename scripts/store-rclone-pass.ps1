# Stores the rclone config password in Windows Credential Manager as nebula/rclone_config_pass
# (PHASE0_PLAN 7.1), then checks that it opens F:\Nebula\config\rclone.conf.
# The password is read from a hidden prompt so it never lands in shell history or process args.
$ErrorActionPreference = 'Stop'

$target = 'nebula/rclone_config_pass'
$conf = 'F:\Nebula\config\rclone.conf'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NebulaCred {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CREDENTIAL {
        public int Flags; public int Type; public string TargetName; public string Comment;
        public long LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob;
        public int Persist; public int AttributeCount; public IntPtr Attributes;
        public string TargetAlias; public string UserName;
    }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CredWrite(ref CREDENTIAL cred, int flags);

    public static void Write(string target, string user, string secret) {
        byte[] blob = System.Text.Encoding.Unicode.GetBytes(secret);
        var c = new CREDENTIAL { Type = 1, TargetName = target, UserName = user, Persist = 2,
                                 CredentialBlobSize = blob.Length,
                                 CredentialBlob = Marshal.AllocHGlobal(blob.Length) };
        try {
            Marshal.Copy(blob, 0, c.CredentialBlob, blob.Length);
            if (!CredWrite(ref c, 0)) throw new System.ComponentModel.Win32Exception();
        } finally { Marshal.FreeHGlobal(c.CredentialBlob); }
    }
}
'@

if (-not (Test-Path $conf)) { throw "$conf not found; run 'rclone config --config $conf' first" }

$secure = Read-Host -Prompt 'rclone config password (input hidden)' -AsSecureString
$plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
    [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
if (-not $plain) { throw 'Empty password.' }

# Verify before storing: rclone reads the password from RCLONE_CONFIG_PASS.
$env:RCLONE_CONFIG_PASS = $plain
try {
    $remotes = & rclone listremotes --config $conf --ask-password=false 2>&1
    if ($LASTEXITCODE -ne 0) { throw "rclone could not open $conf with that password: $remotes" }
} finally {
    Remove-Item Env:RCLONE_CONFIG_PASS -ErrorAction SilentlyContinue
}
foreach ($r in 'gdrive:', 'gdrive-crypt:') {
    if ($remotes -notcontains $r) { throw "Remote $r is missing from $conf (found: $($remotes -join ', '))" }
}

[NebulaCred]::Write($target, 'nebula', $plain)
$plain = $null
Write-Host "Stored as '$target'. Remotes: $($remotes -join ', ')"
