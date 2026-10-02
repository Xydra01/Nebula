# Classic token with only the public_repo scope: fine-grained tokens can't reach repos the
# bot merely collaborates on (github/roadmap#601).
# Stores the Nebula-dev-bot GitHub token in Windows Credential Manager as
# nebula/github_bot_token (Phase 0 task 2.3), then verifies it against the GitHub API.
# The token is read from a hidden prompt so it never lands in shell history or process args.
$ErrorActionPreference = 'Stop'

$target = 'nebula/github_bot_token'
$user = 'Nebula-dev-bot'

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
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CredRead(string target, int type, int flags, out IntPtr cred);
    [DllImport("advapi32.dll")]
    public static extern void CredFree(IntPtr cred);

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

    public static string Read(string target) {
        IntPtr p;
        if (!CredRead(target, 1, 0, out p)) throw new System.ComponentModel.Win32Exception();
        try {
            var c = (CREDENTIAL)Marshal.PtrToStructure(p, typeof(CREDENTIAL));
            return Marshal.PtrToStringUni(c.CredentialBlob, c.CredentialBlobSize / 2);
        } finally { CredFree(p); }
    }
}
'@

$secure = Read-Host -Prompt "Paste the $user token (input hidden)" -AsSecureString
$plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
    [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
if (-not ($plain.StartsWith('ghp_') -or $plain.StartsWith('github_pat_'))) {
    throw 'That does not look like a GitHub token (expected ghp_... or github_pat_...).'
}

[NebulaCred]::Write($target, $user, $plain)
$plain = $null

$token = [NebulaCred]::Read($target)
$headers = @{ Authorization = "Bearer $token"; 'X-GitHub-Api-Version' = '2022-11-28' }
$resp = Invoke-WebRequest -Uri 'https://api.github.com/user' -Headers $headers -UseBasicParsing
$me = $resp.Content | ConvertFrom-Json
$repo = Invoke-RestMethod -Uri 'https://api.github.com/repos/Xydra01/Nebula' -Headers $headers
$token = $null

Write-Host "Stored as '$target'."
Write-Host "Token authenticates as: $($me.login)"
Write-Host "Token scopes: $($resp.Headers['X-OAuth-Scopes'])"
Write-Host "Repo access: push=$($repo.permissions.push) admin=$($repo.permissions.admin)"
