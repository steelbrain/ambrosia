# Installing a guest and finding its ECID

Ambrosia boots an installed macOS. It doesn't install macOS, and it can't
boot recoveryOS, so a guest is installed under Virtualization.framework
first, on an Apple silicon Mac, and then booted on QEMU with Ambrosia. This
page shows what to set up in the guest, which of its files QEMU needs, and
where to find its ECID.

## Install macOS

Use any tool built on Virtualization.framework that installs macOS from a
restore image (an IPSW). Each such tool creates the same three things,
which are all QEMU needs:

- the guest's disk image, often named `disk.img`;
- its auxiliary storage, often `aux.img`, where the guest keeps NVRAM;
- its machine identifier, which holds the ECID.

QEMU reads both images as raw files, so if a tool wrote the disk in
another format, convert it to a raw image first.

The tested builds are 12.6, 13.6, 14.6.1, 15.6.1, 26.6.2 and 27.0.1. macOS
11 can't be a guest: no macOS 11 restore image has a build for the
`vmapple` machine.

## Set it up under Virtualization.framework

Boot the guest once under Virtualization.framework, through the same tool,
and set it up there. On QEMU it has no display that macOS drives, so
Setup Assistant can't be finished there.

1. Finish Setup Assistant and create a user.
2. Turn on Remote Login, in System Settings > General > Sharing (System
   Preferences > Sharing on macOS 12), so that the guest answers SSH.
   With no display, SSH is how you reach it and how you tell that a boot
   worked.
3. Leave System Integrity Protection as it is. Ambrosia boots with it
   enabled, and nothing needs it off.
4. Shut the guest down from inside macOS, so its disks are clean.

The tested guests have FileVault off. A guest with FileVault on has not
been tried.

## Put the files in one directory

`scripts/run-guest.sh` takes a directory that holds `disk.img` and
`aux.img`. Copy or link the guest's files there under those names. On its
first run the script writes `aux.img.trimmed` beside them, the aux image
without its first 16 KiB block, which is what QEMU takes
([the disks](../running.md#the-disks)). From then on the guest keeps its
NVRAM in the trimmed copy.

A guest's disk changes as it runs. To keep the installed guest as it is,
boot a clone of its files: `cp -c` makes one on APFS without copying the
data.

```sh
mkdir vm
cp -c /path/to/guest/disk.img vm/disk.img
cp -c /path/to/guest/aux.img vm/aux.img
```

## Find the ECID

The ECID is a 64-bit number that identifies the guest's machine. QEMU's
`vmapple` takes it as its `uuid` property, and Ambrosia gives it to macOS.
Give the ECID the guest was installed with.

Virtualization.framework keeps it in the VM's machine identifier: the data
of a `VZMacMachineIdentifier`, its `dataRepresentation`, is a binary
property list with one key, `ECID`. Tools save that data, as a file or
encoded in their own configuration. Given it in a file, `plutil` prints the
ECID in decimal:

```sh
plutil -extract ECID raw machine-identifier.bin
```

If a tool stores the data base64-encoded inside its own configuration
file, decode it to a file first:

```sh
printf '%s' "$BASE64_DATA" | base64 -d > machine-identifier.bin
```

QEMU takes the ECID in decimal, or in hexadecimal with a `0x` prefix, and
Ambrosia logs it in hexadecimal on its second line:

```text
ambrosia: 4 CPUs, RAM 0x100000000-0x300000000, ECID 0x<ECID>
```

## Boot it

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin \
    -q /path/to/qemu-system-aarch64 vm
ssh -p 10022 user@127.0.0.1 csrutil status
```

[Running under HVF](run-under-hvf.md) and
[running under TCG](run-under-tcg.md) cover each accelerator, and
[reading the boot log](troubleshoot-a-boot.md) what to do when the guest
doesn't come up.
