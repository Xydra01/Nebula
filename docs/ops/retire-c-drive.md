# Retiring the Old `C:` Drive

The old WD Green 240 GB SATA SSD (`C:`) is past its rated write endurance. [PHASE0_PLAN](../PHASE0_PLAN.md) Section 3.1 has the background.

**Status (2026-10-01): steps 1–6 are done.** The boot loader is on the NVMe, and the drive's SATA port is disabled in BIOS. Windows boots without it. What's left is optional: physically removing the drive. This page records the procedure so it can be repeated, for example on a rebuilt machine, and lists the remaining steps.

This is **hands-on, tier-3 work**. Nebula never does any of it.

## Before you start

- A recent backup: `nebula backup list` shows one from today, and `nebula doctor` shows `backup` as ok.
- The Windows recovery USB is to hand and known to boot (setup.md Section 1).
- About 30 minutes, and access to the BIOS (ASUS PRIME Z490-P: press Del at power-on).

## Moving the boot loader (done 2026-10-01)

1. **Make room on the NVMe.** In Disk Management, shrink `F:` by ~300 MB.
2. **Create an EFI System partition** in the free space (elevated `diskpart`):

   ```text
   list disk
   select disk <NVMe number>
   create partition efi size=300
   format quick fs=fat32 label="System"
   assign letter=S
   ```

3. **Write the boot files** (elevated): `bcdboot F:\Windows /s S: /f UEFI`
4. **BIOS:** put "Windows Boot Manager (Samsung SSD 980)" first in the boot order. Boot with the old drive still connected.
5. **Check** (elevated PowerShell):

   ```powershell
   Get-Disk | Select Number, FriendlyName, IsBoot, IsSystem   # the Samsung 980 is IsBoot and IsSystem
   bcdedit /enum firmware                                       # the boot manager entry points at the new partition
   ```

   Remove the `S:` letter afterwards (`diskpart`: `select volume S`, `remove letter=S`).
6. **Disable the old drive's SATA port in BIOS**, then boot. Only the Samsung 980 and the WD Blue should appear in `Get-Disk`. `nebula doctor` should show no path on `C:` and the EFI partition on the NVMe.

If Windows doesn't boot after step 6, re-enable the port: the old drive still has its original 100 MB EFI partition, so the PC boots as before. Then repeat steps 3–5. If neither works, boot the recovery USB, open a command prompt and re-run step 3 with the NVMe's EFI partition.

## Remaining (optional)

7. **Leave it for a while.** Use the PC normally for a few weeks with the port disabled. Nothing has needed the drive since 2026-10-01.
8. **Wipe it, if it's leaving the machine.** It holds the old Windows install. Re-enable the port and boot. Then, in Disk Management, confirm which disk is the 240 GB WD Green. Check the disk number twice, because a mistake here wipes the wrong drive. Then run `diskpart`: `select disk <WD Green number>`, `clean`. The NVMe's EFI partition is what boots the PC, so cleaning the old drive doesn't affect booting.
9. **Remove it.** Power off, unplug the PC, and disconnect the drive's SATA data and power cables. Re-enable the SATA port in BIOS if you want it for a future drive.
10. **Update the records:** setup.md Section 0, and a dated line in `phase0-notes.md`.
