# Replacing the NVMe (`F:`)

The Samsung 980 (`F:`) holds Windows, the boot loader, your user profile and all hot Nebula data. It had 2 media errors at the 2026-10-01 baseline. The plan is to keep it for about a year, and to replace it sooner if the errors start climbing ([PHASE0_PLAN](../PHASE0_PLAN.md) Section 10).

## When to replace it

- `nebula doctor` shows `smart.<nvme>` WARN, meaning media errors rose since the last reading, or FAIL, meaning available spare fell below its threshold.
- `nebula doctor` shows repeated new disk error events on `F:`.
- Windows reports file corruption, or `chkdsk F:` finds bad sectors.

One warning is a reason to plan, not to panic. Take a SMART snapshot (`F:\Nebula\setup\smart-snapshot.ps1`) a week later. If the count is still rising, replace the drive.

## What you need

- A new NVMe of at least 1 TB. The motherboard (ASUS PRIME Z490-P) has two M.2 slots, so both drives can be installed at once for cloning.
- The Windows recovery USB.
- **From paper:** the rclone config password, and the crypt password and salt (`docs/ops/backup.md`).
- Your Microsoft account password.
- A fresh backup: run `nebula backup now` and check `nebula backup list` shows it in the cloud.

## Option A: clone (preferred while the old drive still reads)

Everything carries over: Windows, apps, drive letters, Nebula, models and scheduled tasks.

1. Install the new drive in the second M.2 slot and boot.
2. Clone the whole disk, including the EFI partition, to the new drive. Use a disk-cloning tool such as Clonezilla (boot it from USB) or the new drive vendor's migration tool. Check that the target is the **new** disk.
3. Power off, take out the old drive (or disable its slot), and boot. In BIOS, put "Windows Boot Manager" on the new drive first if needed.
4. Check:

   ```powershell
   Get-Disk | Select Number, FriendlyName, IsBoot, IsSystem   # the new drive is IsBoot and IsSystem
   nebula doctor                                               # all ok; the SMART check now sees the new drive
   ```

5. Take a SMART baseline of the new drive with `smart-snapshot.ps1`, and note it in `phase0-notes.md`.

If the clone fails because of read errors, the old drive is too far gone. Use option B.

## Option B: fresh install and restore

1. Install the new drive. Boot the recovery USB and install Windows 11 on it.
2. In Disk Management, give the new system drive the letter **`F:`** if possible. Otherwise, update the paths (step 5). Check that `D:` is still the WD Blue: `D:\NebulaCold\` survives untouched, including the local backups and the model archive.
3. Follow [setup.md](setup.md) from Section 1. It rebuilds the toolchain, the repo, the runtimes (pinned and hash-checked) and the models (about 33 GB to download).
4. **Restore Nebula's state and config** from the latest backup:
   - **If `D:\NebulaCold\backups-local\` has a recent copy**, use it: install Nebula (setup.md Section 8a), then run `nebula backup restore <id> --in-place` with the daemon stopped.
   - **Otherwise, restore from Drive:**
     1. Recreate the rclone remotes as in [backup.md](backup.md) "Restore on a new machine": a new `gdrive` sign-in, and `gdrive-crypt` with **the same crypt password and salt from paper**.
     2. Download the newest backup.
     3. Run `nebula backup restore <id> --in-place`. This also brings back the encrypted `rclone.conf`; its config password is on paper.
5. **Paths:** if the drive letters changed, edit `F:\Nebula\config\nebula.toml` (or the restored copy) for `[paths]`, `[model.runtimes]`, the profile model paths and `[backup]`. Then run `nebula doctor`; any path it can't find shows up there.
6. **Credentials and tasks:**
   - `scripts\store-rclone-pass.ps1` (the config password).
   - `nebula backup reauth` (Google sign-in).
   - `scripts\register-backup-tasks.ps1` (elevated).
   - `scripts\phase0-ssh-setup.ps1` (elevated) for SSH over Tailscale. Reinstall Tailscale and sign in first.
   - The SMART scheduled tasks from `scripts\phase0-ws1-admin.ps1`.
7. **Check:**
   - `nebula daemon start`, then `nebula chat`, then `nebula doctor`: all ok, including `backup` after the first scheduled run.
   - A SMART baseline of the new drive.

## Afterwards

- Keep the old drive, unwiped, for a couple of weeks in case something didn't carry over. Then wipe it (`diskpart` `clean` on the right disk) or destroy it.
- Update setup.md Section 0 and `phase0-notes.md` with the new drive and the date.
