# Backups

`nebula backup` snapshots Nebula's state and config, keeps a local copy on `D:` and uploads an encrypted copy to Google Drive through rclone (PHASE0_PLAN 7.1).

## What is backed up

- `F:\Nebula\state\` (`paths.state`): the SMART history, the model-hash cache and, from Phase 1, the database. `doctor.json`, `backup-auth.json` and `backup-last.json` are left out, because they change on their own.
- `F:\Nebula\config\` (`paths.config`): the `nebula.toml` override and the encrypted `rclone.conf`.

Each backup is one `nebula-<UTC time>.tar.zst` with a `MANIFEST.json` listing every file's size and SHA-256. A restore checks every file against it.

## Where it goes

- **Local:** `D:\NebulaCold\backups-local\`, kept for 7 days. The newest copy is always kept. A `.partial` file older than an hour is left over from a run cut off by a shutdown, and the next backup deletes it.
- **Cloud:** `gdrive-crypt:`, an rclone `crypt` remote over `gdrive:NebulaBackups`. Google only sees encrypted names and contents.
- **Schedule:** Task Scheduler runs `nebula backup now --if-changed` at 00:00, 06:00, 12:00 and 18:00 (skipped when nothing changed), and `nebula backup now` at 03:00 (always uploads). Missed runs start when the PC is next on.
- **Retention (approved 2026-10-04):** in the cloud, the newest 8 backups plus the newest of each of the last 14 days, 8 ISO weeks and 6 months. If a pass would delete more than 500 files or 1 GB, it stops and records an error until you re-run it with `--allow-large-delete`.

## Secrets

| Secret | Where | Needed for |
| --- | --- | --- |
| Crypt password and salt (password2) | **On paper, offline** | Decrypting the backups on any machine. Without them the cloud copies are unreadable. |
| rclone config password | On paper, and in Credential Manager as `nebula/rclone_config_pass` | Opening `F:\Nebula\config\rclone.conf` on this PC |
| Google OAuth client ID and secret | Inside `rclone.conf` (Google Cloud project `nebula-backup`) | Signing in to Drive |

## Weekly sign-in renewal

The Google app is unpublished, because publishing needs a verified domain. Google therefore ends its sign-in **7 days** after it was given. `nebula doctor` shows `backup.auth`: ok, then WARN 2 days before expiry, then FAIL once expired. When it warns, run this at the desktop:

```powershell
nebula backup reauth
```

Answer `y` to "Already have a token - refresh?", then sign in and allow access in the browser. The command checks that Drive accepts the new sign-in and records the time. If the sign-in lapses, the local copies on `D:` continue, and the next run after renewal uploads again.

To end the weekly renewal, either publish the app (see "Setup" below) or move to a remote with a permanent key, then set `token_lifetime_days = 0` under `[backup]` in `nebula.toml`.

## Commands

```powershell
nebula backup now                    # back up, upload, apply retention
nebula backup list                   # local and cloud backups
nebula backup restore <id>           # unpack and verify into D:\NebulaCold\restore\<id>
nebula backup restore <id> --to DIR  # somewhere else
nebula backup restore <id> --in-place  # overwrite live state and config (daemon stopped)
nebula backup reauth                 # renew the Drive sign-in
```

A restore uses the local copy if there is one; otherwise it downloads the backup from the cloud. `--in-place` only writes the files in the backup, and never deletes anything.

## Setup (done 2026-10-04)

1. **Google Cloud project** (any Google account): enable the Google Drive API. Under the OAuth consent screen, choose External, with the only scope `.../auth/drive.file`, no logo, and the app left in Testing. Create a **Desktop app** client.
2. **rclone remotes:** run `rclone config --config F:\Nebula\config\rclone.conf`.
   - `gdrive`: type `drive`, your client ID and secret, scope `drive.file`, sign in through the browser. Google shows an "unverified app" warning; continue past it.
   - `gdrive-crypt`: type `crypt`, remote `gdrive:NebulaBackups`, standard filename encryption, encrypted directory names. Generate the password and salt, and **write both down**.
   - Set a configuration password (`s`) and **write it down**.
3. `scripts\store-rclone-pass.ps1`: checks the config password and saves it to Credential Manager.
4. `nebula backup reauth --record-only`: records the sign-in for doctor.
5. `scripts\register-backup-tasks.ps1` from an elevated PowerShell: registers the two tasks under `\Nebula\`, using your Windows password so they run while you're logged out. For a Microsoft account this is the **account password, not the PIN**. With `-WhenLoggedOn` no password is needed, but backups only run while you're logged in, and missed ones start at your next logon.

## Restore on a new machine

1. Install rclone. Create a `gdrive` remote (same steps as above), then a `gdrive-crypt` remote using **the crypt password and salt from paper**.
2. `rclone lsf gdrive-crypt:` lists the backups. `rclone copy gdrive-crypt:<name> .` downloads one.
3. Unpack it with `nebula backup restore` once Nebula is installed, or with `tar --zstd -xf <name>`. Copy `state\` and `config\` into place, and check the files against `MANIFEST.json`.

## Test restore log

| Date | Backup | Result |
| --- | --- | --- |
| 2026-10-04 | `nebula-20261004T171414Z` (5 files, cloud only: the local copy was moved aside) | Downloaded from `gdrive-crypt:` byte-identical to the local copy. All 5 files matched the manifest and the live files. On Drive the file appears only under an encrypted name. |
