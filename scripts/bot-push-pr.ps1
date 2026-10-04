# Pushes the current branch as Nebula-dev-bot and opens a PR against main.
# The token is read from Credential Manager and passed to git through GIT_CONFIG_* env vars,
# so it never appears in process arguments, the remote URL, or git config.
param(
    [Parameter(Mandatory)] [string] $Title,
    [Parameter(Mandatory)] [string] $Body,
    [string] $Repo = 'Xydra01/Nebula',
    [string] $Base = 'main'
)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class NebulaCredRead {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CR { public int Flags; public int Type; public string TargetName; public string Comment;
        public long LastWritten; public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist;
        public int AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName; }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CredRead(string t, int ty, int f, out IntPtr c);
    [DllImport("advapi32.dll")] static extern void CredFree(IntPtr c);
    public static string Read(string t) {
        IntPtr p; if (!CredRead(t, 1, 0, out p)) throw new System.ComponentModel.Win32Exception();
        try { var c = (CR)Marshal.PtrToStructure(p, typeof(CR));
              return Marshal.PtrToStringUni(c.CredentialBlob, c.CredentialBlobSize / 2); }
        finally { CredFree(p); }
    }
}
'@

$token = [NebulaCredRead]::Read('nebula/github_bot_token')
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -eq $Base) { throw "Refusing to push $Base as the bot." }

$basic = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("x-access-token:$token"))
$env:GIT_CONFIG_COUNT = '1'
$env:GIT_CONFIG_KEY_0 = 'http.https://github.com/.extraheader'
$env:GIT_CONFIG_VALUE_0 = "AUTHORIZATION: basic $basic"
try {
    git push "https://github.com/$Repo.git" "HEAD:refs/heads/$branch"
    if ($LASTEXITCODE -ne 0) { throw "git push failed ($LASTEXITCODE)" }
} finally {
    Remove-Item Env:GIT_CONFIG_COUNT, Env:GIT_CONFIG_KEY_0, Env:GIT_CONFIG_VALUE_0 -ErrorAction SilentlyContinue
}

$headers = @{ Authorization = "Bearer $token"; 'X-GitHub-Api-Version' = '2022-11-28' }
$payload = @{ title = $Title; body = $Body; head = $branch; base = $Base } | ConvertTo-Json
# Windows PowerShell 5.1 encodes a string -Body as ISO-8859-1, which breaks non-ASCII text.
$pr = Invoke-RestMethod -Method Post -Uri "https://api.github.com/repos/$Repo/pulls" -Headers $headers `
    -Body ([Text.Encoding]::UTF8.GetBytes($payload)) -ContentType 'application/json; charset=utf-8'
$token = $null
Write-Host "PR #$($pr.number) opened by $($pr.user.login): $($pr.html_url)"
