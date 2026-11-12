# The restore boot

Ambrosia normally boots a guest's installed system. Given a *restore bundle*
instead, it boots the installer's restore OS: the ramdisk and kernel an IPSW
carries for restoring a machine. Ambrosia does the part iBEC does on a real
restore — it places the restore objects in memory and enters the restore
kernel — so that Apple's own restore OS runs in the guest and writes the
guest's LocalPolicies itself.

Ambrosia runs no AVPBooter, iBSS or iBEC. Only the Apple code the bundle
carries, the restore objects from the user's own IPSW, runs. Nothing Apple
ships is built into Ambrosia or committed to it.

## The restore bundle

A restore bundle is a cpio archive in the same "newc" format as a
[configuration bundle](configuration.md#the-bundle), loaded into RAM the same
way (`-device loader,file=<bundle>,addr=0x120000000,force-raw=on`). Ambrosia
takes the restore path when the bundle holds a restore kernelcache; a
configuration bundle never does, so the two are exclusive and Ambrosia needs
no flag to tell them apart.

The bundle's members, each named for its role:

| Member | What it is |
|---|---|
| `restore-kernelcache.im4p` | The restore kernelcache, an IM4P (`krnl`). |
| `restore-devicetree.im4p` | The restore device tree, an IM4P (`dtre`). |
| `restore-trustcache.im4p` | The restore trust cache, an IM4P (`rtsc`). |
| `apticket.der` | The restore AP ticket, the DER of an IM4M. |
| `restore-policy.img4` | The restore LocalPolicy, an IMG4. |
| `nonce` | The 32-byte AP nonce the ticket is bound to. |
| `boot-args` | The restore kernel's command line, such as `rd=md0 -restore`. |

The ramdisk, an IM4P (`rdsk`) whose payload is the raw APFS container the
restore OS mounts as `md0`, rides on its own loader object
(`-device loader,file=<ramdisk>,addr=0x180000000,force-raw=on`), since it is
hundreds of megabytes.

The restore kernelcache and device tree are the installed system's own
images, so the restore boot reuses the kernel-collection load, the
pointer-authentication spins and the device-tree trimming of an installed
boot.

Building a restore bundle from an IPSW, and driving the restore protocol over
the machine's [USB device controller](machine.md#a-usb-device-controller-for-a-restore-boot),
is a separate tool's work. This page describes only what Ambrosia does with
the bundle it is given.

## What Ambrosia does

The [boot process](boot-process.md#the-restore-boot) gives the steps beside
the installed system's. In short, Ambrosia:

- places the restore kernelcache at a random page in RAM's first 32 MiB, with
  the trust cache in the block right below it, as it places the installed
  system's: the restore kernel panics on a trust cache above the collection;
- places the device tree and the boot arguments above the collection, and the
  ramdisk below the end of the kernel's data, so the kernel keeps it rather
  than freeing it;
- names the ramdisk in the device tree's memory map as `RAMDisk`, so the
  kernel mounts it as `md0`, the root the boot arguments name with `rd=md0`;
- fills the device tree as for an installed boot, but keeps the `avp-ctrr`
  node the restore kernel reaches its OS with, and leaves out the
  sealed-system-volume fields a ramdisk boot has none of;
- marks the boot a remote boot in `/chosen`, as iBEC's restore does — the boot
  command, the `remote` boot type, the handoff boot type and the upgrade
  environment — so the kernel's boot policy takes its restore path and does not
  evaluate an installed system's local policy;
- reads the guest's NVRAM from the aux disk, as an installed boot does, since
  the restore OS needs it and rewrites it; the aux must carry a valid NVRAM
  bank, which the restore tooling writes, or the boot stops;
- enters the restore kernel.

Ambrosia applies no configuration on the restore path: no `config.plist`, no
patches and no kexts. The restore LocalPolicy is signed by the production
chain, so it needs none.

## The machine

A restore boot needs a machine with the USB device controller the restore OS
reaches its host through
([the machine](machine.md#a-usb-device-controller-for-a-restore-boot)). The
rest of the machine is what an installed boot needs.
