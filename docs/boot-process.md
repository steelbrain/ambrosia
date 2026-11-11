# The boot process

What Ambrosia does from reset to the kernel's first instruction, in the
order it does it. Ambrosia takes the place of both AVPBooter and iBoot: no
Apple code runs before the kernel. It reads the boot objects iBoot would
load, lays them out as iBoot does, and checks no signatures.

[`crates/firmware/src/main.rs`](../crates/firmware/src/main.rs) is the
sequence. The firmware crate holds what touches the machine, and the core
library, [`crates/ambrosia`](../crates/ambrosia/README.md), does the rest.
[Replacing iBoot](internals/replacing-iboot.md) explains why each step is
the way it is, and [lessons learned](internals/lessons-learned.md) lists
what each one took to find.

1. [Reset](#reset)
2. [The identity map and the heap](#the-identity-map-and-the-heap)
3. [The configuration](#the-configuration)
4. [The graphics driver module](#the-graphics-driver-module)
5. [Reading the boot objects](#reading-the-boot-objects)
6. [The pointer authentication keys](#the-pointer-authentication-keys)
7. [Unpacking them](#unpacking-them)
8. [NVRAM and the clock](#nvram-and-the-clock)
9. [The memory layout](#the-memory-layout)
10. [The PAC nop pass](#the-pac-nop-pass)
11. [The configuration's patches](#the-configurations-patches)
12. [Placing the kexts](#placing-the-kexts)
13. [The device tree](#the-device-tree)
14. [The boot arguments](#the-boot-arguments)
15. [Entering the kernel](#entering-the-kernel)

[The log](#the-log) covers what Ambrosia prints throughout.

## Reset

The machine starts the boot CPU at 0x00100000, the start of the image, at
EL1 with the MMU and caches off. The other CPUs stay off until the kernel
starts them through PSCI, which the VMM handles.

`_start`, in [`start.s`](../crates/firmware/src/start.s), masks interrupts,
turns on FP and SIMD, sets the exception vectors and the stack, zeroes the
bss and calls `main`. The image, its data, its bss and its 64 KiB stack all
live in the 1 MiB firmware region, which is RAM. The machine reloads the
image's bytes on reset but not the rest of the region, so the bss is zeroed
by hand.

`main` logs the firmware's version, then what the machine's config block
reports: the CPU count, the RAM range and the ECID.

## The identity map and the heap

Ambrosia turns on the MMU with an identity map
([`mmu.rs`](../crates/firmware/src/mmu.rs)), so that it runs with caches on
and may make unaligned accesses. With the MMU off, all memory is treated as
device memory, where an unaligned access faults.

- The firmware region and the config block, below 0x00600000, are normal
  write-back memory.
- The rest of the first 4 GiB, devices and the PCIe window, is device
  memory (nGnRnE) and not executable.
- RAM, from 4 GiB, is normal write-back memory and executable.

The heap is a bump allocator over RAM's second GiB, from RAM's start plus
1 GiB to plus 2 GiB, or to 64 MiB below RAM's end if that is lower. It
never frees, except to shrink or grow its most recent allocation. Every
file read from the disks lives there. The kernel's static memory fits
below it, and the carveouts at the top of RAM above it.

## The configuration

Ambrosia looks for a configuration bundle at 0x120000000, 512 MiB into
RAM, where QEMU's generic loader puts one. If memory there starts with a
cpio "newc" header, it parses the bundle and its `config.plist`
([`bundle.rs`](../crates/ambrosia/src/bundle.rs),
[`config.rs`](../crates/ambrosia/src/config.rs)) and logs what it found:

```text
ambrosia: configuration: 14748 bytes, 0 kexts, 0 patches, graphics driver Drivers/bochs-display.elf
```

Otherwise it boots with the default: no kernel command line, kexts,
patches or driver module. A bundle it can't use stops the boot.
[Configuration](configuration.md) has the format.

The bundle sits above the kernel's static memory and below the heap, so
nothing overwrites it while Ambrosia runs. Once the kernel runs, that RAM
is the kernel's.

## The graphics driver module

If the configuration names a graphics driver module, Ambrosia starts it
now, before any disk read, so its console shows the rest of the boot
([`driver.rs`](../crates/firmware/src/driver.rs)):

1. The loader ([`elf.rs`](../crates/ambrosia/src/elf.rs)) checks the ELF
   file and finds the memory it needs.
2. Ambrosia allocates that memory in the heap, aligned to the module's
   largest segment alignment, copies the segments in, applies the
   relocations, and makes the instruction cache coherent with the code.
3. It calls the entry point once, with the PCIe ECAM, the PCIe window and
   a log callback.
4. If the module returns 0 with a framebuffer that passes Ambrosia's
   checks, Ambrosia clears it to black and draws every later log line on
   it as well as on the UART
   ([`console.rs`](../crates/ambrosia/src/console.rs)).

Ambrosia never calls the module again. [Graphics drivers](graphics-drivers.md)
is the interface for module authors.

## Reading the boot objects

Ambrosia reads the guest's disks through the machine's firmware disk
interface ([`bdif.rs`](../crates/firmware/src/bdif.rs)), which gives it the
root and aux drives. It never writes to them.
[`files.rs`](../crates/ambrosia/src/files.rs) finds the objects, in the
places iBoot finds them.

**The root disk's partitions.** Ambrosia reads the GPT's primary header at
LBA 1 and its partition entry array, and checks each against its CRC32
([`gpt.rs`](../crates/ambrosia/src/gpt.rs)). The backup table is not read.
The disk must have exactly one partition of each of these types:

- the main APFS container, `7C3457EF-0000-11AA-AA11-00306543ECAC`;
- the iBoot system container, `69646961-6700-11AA-AA11-00306543ECAC`.

The recoveryOS container, `52637672-7900-11AA-AA11-00306543ECAC`, is not
read.

**APFS.** A read-only reader ([`apfs`](../crates/ambrosia/src/apfs/mod.rs))
opens each container at its latest valid checkpoint, reads the container's
and each volume's object map, and walks the file-system B-trees. Every
object it reads is checked against its Fletcher-64 checksum and against the
type, object ID and transaction its reference implies, and every walk is
bounded. Encrypted volumes, snapshots and compressed files are not
supported and give an error. The boot objects need none of them: they sit
in Preboot volumes, uncompressed.

**The main container.** The System volume's volume group, G, names the
macOS install. In the Preboot volume, the file `/G/boot/active` holds H,
the SHA-384 of the install's boot manifest, in hex. Ambrosia reads these
files from `/G/boot/H/`:

- `System/Library/Caches/apticket.der`, the boot manifest (IM4M). The
  Preboot volume keeps a copy in `/G/restore/` as well, as
  `apticket.vma2macosap.<ECID>.im4m`, beside other tickets, and a disk
  built without macOS's installer can have only that copy. Where the boot
  directory has no `apticket.der`, Ambrosia takes the `apticket.*.im4m` in
  `/G/restore/` whose SHA-384 is H, and logs
  `boot manifest: /G/restore/<file>, whose SHA-384 names the boot directory`;
- `System/Library/Caches/com.apple.kernelcaches/kernelcache`, the kernel
  collection;
- `usr/standalone/firmware/devicetree.img4`, the device tree template;
- `usr/standalone/firmware/root_hash.img4` and
  `base_system_root_hash.img4`, the root hashes of the sealed system volume
  and of BaseSystem;
- `usr/standalone/firmware/FUD/StaticTrustCache.img4` and
  `BaseSystemTrustCache.img4`, the trust caches of the system and of
  BaseSystem.

**The iBoot system container.** Its Preboot volume holds the install's
LocalPolicy, as `/G/LocalPolicy/<hash>.img4`, beside a
`<hash>.recovery.img4` that recoveryOS boots with, which Ambrosia skips. It
also holds the SFR manifest, `/SFR/current/apticket.der`.

**The aux disk.** The device tree template's `/product/util` node has a
`regions` table that lists the aux disk's regions as (id, offset, size)
triples of 32-bit values. NVRAM is region 2: 1 MiB at 0xA00000 on the
tested guests. Ambrosia reads it whole.

Ambrosia asks the machine for at most 16 MiB a request. A 27.0.1 guest's
objects take 412 requests, 27.5 MB in all. The log names the install and
its boot directory:

```text
ambrosia: macOS <volume group UUID>, boot objects <manifest hash>
```

## The pointer authentication keys

XNU on `vmapple` never writes the pointer authentication key registers, so
the keys the firmware leaves are the ones the kernel and every process run
on for the guest's life. Once it has read the boot objects, Ambrosia
derives five 128-bit keys, IA, IB, DA, DB and GA, from the boot's seed
([randomness](#the-memory-layout)) with SHA-384, so each key differs and
they change from boot to boot. It writes them on the boot CPU before it
lays out the kernel ([`keys.rs`](../crates/firmware/src/keys.rs)). The VMM
starts the other CPUs on the same keys
([the machine](machine.md#secondary-cpus-on-the-boot-cpus-keys)).

## Unpacking them

The objects come in Image4 containers
([`img4.rs`](../crates/ambrosia/src/img4.rs),
[`der.rs`](../crates/ambrosia/src/der.rs)):
an IM4P holds a payload and its type, and an IMG4 holds an IM4P and the
manifest (IM4M) it is signed with. A payload is LZFSE-compressed,
LZSS-compressed or stored as it is. Ambrosia checks no signatures.

- **The kernel collection** is an IM4P. Its payload, LZFSE-compressed, or
  stored as it is on 12.6, is an arm64e `MH_FILESET` Mach-O
  ([`macho.rs`](../crates/ambrosia/src/macho.rs)) whose file offsets equal
  its virtual offsets. So Ambrosia unpacks it straight to where it runs, as
  one block: 216 entries and 0x4dac000 bytes on 27.0.1. Its entry point is
  in its `LC_UNIXTHREAD`. The kernel applies the collection's chained fixups
  itself.
- **The device tree template**, the root hashes and the trust caches are
  IM4Ps. The template is Apple's flattened device tree
  ([`adt.rs`](../crates/ambrosia/src/adt.rs)): each node is a property
  count, a child count, its properties, then its children. A property is a
  32-byte name, a 32-bit length whose top bit marks a value the firmware
  fills in, and its value, padded to 4 bytes.
- **The trust caches** are given to the kernel as one block: a count, each
  module's offset, then the modules.
- **The LocalPolicy** is an IMG4 whose IM4M carries the policy
  ([`im4m.rs`](../crates/ambrosia/src/im4m.rs)).

## NVRAM and the clock

The NVRAM region holds two banks of 512 KiB
([`nvram.rs`](../crates/ambrosia/src/nvram.rs)). Each bank has an Adler-32
at +0x10, over the bank from +0x14 on, and a generation count at +0x14.
The current bank is the valid one with the highest generation. After the
bank's header come its partitions, `common` among them, each a 16-byte
header and NUL-terminated `name=value` entries. Ambrosia hands the current
bank to the kernel whole, as `/chosen/nvram-proxy-data`, and logs which it
is:

```text
ambrosia: NVRAM bank 1 of 2, generation 260
```

macOS's driver for `avp-rtc` keeps the clock's offset to calendar time in
the `common` partition, in microseconds, as `com.apple.System.rtc-offset`.
It writes 1 for an offset of 0. A guest that has never set its clock under
`avp-rtc` has no offset, and its timed then takes the clock for reset and
sets it back to the last write to the disk. The machine's counter counts
calendar time, so when the device tree keeps `avp-rtc` and the bank has no
offset, Ambrosia adds `com.apple.System.rtc-offset=0x1` to the copy it
hands the kernel, fixes the bank's checksum, and logs:

```text
ambrosia: NVRAM has no RTC offset: set it to zero
```

The guest's own offset replaces it the first time the guest sets its
clock.

## The memory layout

[`boot.rs`](../crates/ambrosia/src/boot.rs) lays out RAM as iBoot does.
From the bottom of RAM:

```text
0x100000000  physBase: the start of RAM and of the kernel's memory
             (unused, up to a random page in the first 32 MiB)
             the Auxiliary KC, if the configuration adds a kext
             the trust caches
             the kernel collection
             SEPFW: one empty 16 KiB page
             the device tree
             the boot arguments' 16 KiB page, which ends topOfKernelData
             ...
0x120000000  the configuration bundle
0x140000000  Ambrosia's heap
             ...
             the framebuffer carveout, 640 by 1136 at 32 bits, where
               memSize ends
             the panic log, 0x1fc000 bytes
             the manifests, up to the end of RAM
```

- The kernel collection goes at a random 16 KiB page in RAM's first
  32 MiB, leaving room below it in the same 32 MiB block for the trust
  caches, and for the Auxiliary KC when the configuration adds a kext.
  26.6.2 and 27.0.1 panic on trust caches above the collection, and the
  kernel's VM bootstrap requires the Auxiliary KC's lowest segment to lie
  below the Boot KC's, so both sit below the collection.
- The device tree goes above the collection. Early on, 14.6.1 maps nothing
  below the collection, and 26.6.2 nothing below its 32 MiB block, so a
  tree placed below it can't be read.
- Everything up to `topOfKernelData` must end below the bundle.
- The carveouts at the top of RAM are the framebuffer iBoot gives, the
  panic log and the three manifests: the boot manifest, the LocalPolicy's
  and the SFR manifest, each 64-byte aligned. The kernel's memory,
  `memSize`, ends at the framebuffer.

**KASLR.** The kernel maps itself in 32 MiB blocks, so `virtBase -
physBase` must be a multiple of 32 MiB. Ambrosia picks a slide from 32 MiB
to 512 MiB, in 16 KiB pages, that puts the collection's virtual address at
the same offset in its 32 MiB block as its physical address.

**Randomness.** The boot's seed is a SHA-384 over the machine's random
value, which the VMM draws once per process, and the virtual counter,
which tells boots of one process apart. The seed gives the collection's
place, the slide, the device tree's random fields and the pointer
authentication keys.

The log gives the result:

```text
ambrosia: kernel collection: 216 entries, 0x4dac000 bytes at 0x100a70000
ambrosia: physBase 0x100000000 virtBase 0xfffffe0020000000 top 0x1058b0000 memSize 0x1ffb38000 slide 0x19a6c000
```

## The PAC nop pass

A `vmapple` kernel asks the hypervisor to manage its pointer
authentication keys with `hvc #0` calls, which fail under HVF's public API
and under TCG ([the machine](machine.md#the-pac-hypercalls)). From macOS 14
on, the kernel spins when one fails: `hvc #0; cbnz x0, .`.

Ambrosia replaces each `cbnz x0, .` (0xb5000000) that directly follows an
`hvc #0` (0xd4000002) in the collection's executable segments with a `nop`
(0xd503201f) ([`pac.rs`](../crates/ambrosia/src/pac.rs)). The kernels of
14.6.1 to 27.2.1 have 19 such sites; those of 12.6 and 13.6 have none, and
the pass changes nothing there.

```text
ambrosia: 19 PAC spins made nops
```

The kernel then runs on the keys Ambrosia set, shared by the kernel and
every process, with no per-process B keys and no EL0 diversifier.

## The configuration's patches

Each `Kernel` > `Patch` entry is applied next, over the segments of the
collection entry it names, but `__LINKEDIT`
([configuration](configuration.md#kernel--patch)). Each logs how many
matches it replaced.

## Placing the kexts

When the configuration adds kexts ([`Kernel` >
`Add`](configuration.md#kernel--add)), Ambrosia builds an Auxiliary kernel
collection that holds them all and places it below the kernel collection
([`auxkc.rs`](../crates/ambrosia/src/auxkc.rs),
[`place.rs`](../crates/ambrosia/src/place.rs)). On a Mac a third-party
kext goes in the Auxiliary KC, which the kernel loads once early user space
runs. Building that collection in memory lets the kernel start the kexts
with SIP on, where adding them to the Boot KC would change the collection
whose signature iBoot checks.

The collection is an `MH_FILESET` linked at base zero. The kexts' segments
are merged by name into its top-level `__TEXT`, `__TEXT_EXEC`,
`__DATA_CONST` and `__DATA`, so the kernel maps code executable and data
writable. Each kext gets its own fileset entry and prelink dictionary. An
imported symbol resolves against every member of the kernel collection,
then against the other placed kexts. The kexts' pointers are left as
chained fixups for the kernel's loader to rebase and sign. No code
signature or trust-cache entry is needed, since the guest runs with
developer mode force-enabled, as the machine reports it.
[Replacing iBoot](internals/replacing-iboot.md#the-auxiliary-kernel-collection)
describes the collection in full.

Ambrosia advertises the collection in `/chosen/memory-map` as `AuxKC` and
`AuxKC-mach_header`, as the Boot KC is advertised. It also records the
kernel collection's base and each placed kext's base and size under
`/chosen/ambrosia`, a node that survives into the I/O Registry, so injected
code can find what was placed ([the handover](internals/handover.md)). The
log gives the result, with a `kext` line for each kext:

```text
ambrosia: AuxKC: <bytes> bytes, 1 kext(s), <count> fixups, BootKCID <data>
ambrosia: AuxKC: kext ai.aneesiqbal.ambrosia.example.hello at 0x4000 (slid <address>), code <size>, kmod <offset>
ambrosia: AuxKC placed at phys <address>, virt <address>, <size> bytes, 1 kext(s)
```

When the configuration lists vtable symbols ([`Kernel` >
`ExportVtableDiscriminators`](configuration.md#kernel--exportvtablediscriminators)),
Ambrosia also reads each vtable's per-slot pointer-authentication
discriminators from the kernel collection, before the kernel applies its
fixups, and records them in the handover:

```text
ambrosia: vtable-disc: 2 donor vtable(s), <count> slot(s) in the handover
```

## The device tree

The kernel gets the guest's template with what iBoot would add, from the
same inputs: the config block, the boot objects and manifests, NVRAM and
the memory layout ([`tree.rs`](../crates/ambrosia/src/tree.rs)).

**Template directives.** A property whose length has its top bit set is a
directive for the firmware. `syscfg/SrNm`, `syscfg/Mod#` and `syscfg/Regn`
are filled with the config block's serial number, model and region, and
`zeroes/<len>` with zeroes. Any other, such as `macaddr/...`, is dropped,
as iBoot drops one it has no value for.

**The root.** `platform-name` is `vmapple2`.

**`/chosen`** gets:

- the security state of a Full Security boot, such as `secure-boot`,
  `effective-security-mode-ap` and `effective-production-status-ap` set to
  1, and the other fixed values iBoot writes;
- `chip-id` and `board-id` from the boot manifest's `CHIP` and `BORD`;
  `unique-chip-id`, the ECID; and `unique-device-id`;
- `firmware-version`, `system-firmware-version` and the iBoot stage tags,
  as `Ambrosia-<version>`;
- `random-seed` and `cl4-entropy` from the boot's seed, and `boot-nonce`
  from the machine's random value, as iBoot fills it;
- the install: `apfs-preboot-uuid` and `boot-uuid` (the volume group),
  `boot-objects-path`, `boot-manifest-hash`, `system-volume-auth-blob` (the
  root hash) and `root-snapshot-name` (`com.apple.os.update-<hash>`, from
  the root hash);
- NVRAM: `nvram-proxy-data`, the bank's size and count, and the current
  bank's index;
- `dram-base`, `dram-size`, `embedded-panic-log-size` and `syscfg-v2-data`;
- `manifest-properties`, the boot manifest's own properties, and
  `manifest-object-properties`, those of its `krnl` entry;
- `boot-object-manifests` and `secure-boot-hashes`: where each manifest is
  in memory, and its SHA-384;
- `asmb`: the LocalPolicy's properties, as `lp-<code>`;
- `memory-map`: the SEPFW page, the trust caches, the device tree, the
  boot arguments and the kernel collection's parts, with all-ones ranges
  for objects this machine doesn't have;
- `carveout-memory-map`: the kernel's memory, the panic log, the
  framebuffer and the manifests, by iBoot's region numbers 24, 98, 14 and
  76;
- a `handoff` node, if the template has none.

iBoot also writes breadcrumbs of its own, such as `iboot-handoff` and
`preoslog`. Ambrosia writes none of them, and macOS boots without them.

**`/cpus`** keeps only the CPUs whose `cpu-id` is below the machine's CPU
count. `max_cpus` is the template's count.

**`/arm-io`** loses the nodes of devices the machine doesn't have:

- `gfx` and `iosurface-mapper`, Apple's paravirtual display and IOSurface
  mapper, which Ambrosia doesn't offer;
- `avp-ctrr`, a CTRR device the kernel would read as it starts, an external
  abort under TCG;
- one of the two clocks, as iBoot chooses: `pl031-rtc` when avp-rtc's ID
  register reads 0xf001, and `avp-rtc` otherwise. The 12.6 and 13.6
  templates have only the PL031, named `rtc`, which stays.

`avp-sealed-registers` stays, though the machine lacks the device: without
the node, 27.0.1 never answers SSH. `fuse-revision`, `tunable-revision`
and `chip-config-rack` are set to 0.

When a graphics driver module drives a display and the configuration
names a `Graphics` > `DisplayNode`, `/arm-io` gains a node by that name
whose `device_type` is `display-subsystem`, for a kernel display driver to
attach to ([configuration](configuration.md#graphics--displaynode)).

**The PCIe window.** The template's `/arm-io/pcie` `ranges` give
Virtualization.framework's window. The kernel's PCIe driver reads each
28-byte entry as a memory window at the same address on the bus as in
physical memory, and assigns every BAR from it. Ambrosia writes one entry,
32-bit memory space from 0x50000000, 0xb0000000 bytes long. A device's
memory is its BAR's address resolved through the PCIe node's `ranges`, then
arm-io's. arm-io's template range ends at 0x80000000, and must stay as it
is: with it stretched to 4 GiB, 27.0.1 stops before its first console line.
So Ambrosia adds a second arm-io range, from 0x80000000 to 4 GiB. Ambrosia
assigns no BARs; the kernel assigns each, keeping those a graphics driver
module placed.

**The rest.** `/pram` and `/vram` give the panic log and framebuffer
carveouts. `/product` gets `allow-hactivation` and the config block's SoC
name. `/osenvironments`, the overlays for a recoveryOS boot, is removed.

**Not added: `/filesystems`.** The machine has no Secure Enclave. Booted
by AVPBooter and iBoot, macOS 15 and later panic in APFS on the unencrypted
data volume unless the tree has a `/filesystems` node with `DoNotUseSEP`.
Booted by Ambrosia they don't: without the node, APFS mounts each volume
unencrypted on every tested build. The 12.6 and 13.6 templates have a
`/filesystems` node of their own, which stays.

The tree is written out with zeroes to the end of its last 16 KiB page.

## The boot arguments

XNU's `boot_args`, revision 3, version 2, at the start of their 16 KiB page
([`bootargs.rs`](../crates/ambrosia/src/bootargs.rs)). The structure has
two layouts. The 12.6 and 13.6 kernels read 736 bytes, with a 608-byte
command line. The kernels from 14.6.1 to 27.2.1 read 1152 bytes, with a
1024-byte command line, and the two fields after it lie 416 bytes further
on. Ambrosia writes one 1152-byte block that reads right in both:

| Offset | Field | Value |
|---|---|---|
| 0 | `Revision` | 3 |
| 2 | `Version` | 2 |
| 8 | `virtBase` | The kernel virtual address of `physBase`. |
| 16 | `physBase` | The start of RAM. |
| 24 | `memSize` | From `physBase` to the framebuffer carveout. |
| 32 | `topOfKernelData` | The end of the boot arguments' page. |
| 40 | `Video` | Six 64-bit fields, below. |
| 88 | `machineType` | 0 |
| 96 | `deviceTreeP` | The device tree's kernel virtual address. |
| 104 | `deviceTreeLength` | Its length, 32 bits. |
| 108 | `CommandLine` | `BootArgs`, cut to 607 bytes so that its NUL ends within the shorter field. |
| 720 | `bootFlags`, 736-byte layout | 0 |
| 728 | `memSizeActual`, 736-byte layout | 0, which has the kernel take `memSize`. |
| 1136 | `bootFlags`, 1152-byte layout | 0 |
| 1144 | `memSizeActual`, 1152-byte layout | All of RAM, as `/chosen/dram-size` gives it. The 27.2.1 kernel panics with `inconsistent DRAM size reported by iBoot` where the two differ. |

The shorter layout's two fields lie inside the longer layout's command
line, past its NUL, so they stay zero.

`Video` is `v_baseAddr`, `v_display`, `v_rowBytes`, `v_width`, `v_height`
and `v_depth`. Bits 16 to 23 of `v_depth` hold the scale less 1. Without a
graphics driver module it gives iBoot's framebuffer in RAM: 640 by 1136,
2560 bytes a row, depth 32 at scale 2. Nothing shows that framebuffer. With
a module it gives the module's framebuffer, at depth 32 and scale 1, and
the kernel's console draws there.

## Entering the kernel

Ambrosia cleans and invalidates to the point of coherency every range it
wrote for the kernel, from `physBase` to `topOfKernelData` and from the
framebuffer carveout to the end of RAM, and the part of the heap it used.
No dirty cache line is then left to be written back over memory the kernel
writes with its caches off.

`enter_kernel`, in [`start.s`](../crates/firmware/src/start.s), turns the
MMU and caches off, invalidates the instruction cache and the TLB, and
zeroes `VBAR_EL1`, `TTBR0_EL1` and `TCR_EL1`. It enters the kernel in the
state iBoot enters it:

- on the boot CPU alone, at EL1, with D, A, I and F masked;
- with the MMU and caches off, and FP enabled;
- with x0 at the boot arguments' physical address, x30 and the PC at the
  collection's entry point, and every other general register zero.

```text
ambrosia: entering the kernel at 0x103920480
```

## The log

Every line goes to the machine's UART, a PL011 at 0x20010000, starting
with `ambrosia: `, and to the early console once a driver module has given
one ([`uart.rs`](../crates/firmware/src/uart.rs)). The kernel prints
nothing to the UART unless its boot arguments ask, with `serial=3`.

An error stops the boot with a line that says what failed
([reading the boot log](how-to/troubleshoot-a-boot.md#when-the-boot-stops)),
and the boot CPU waits for an interrupt that never comes. An exception in
the firmware or in a driver module logs the vector's index, `ESR_EL1`,
`ELR_EL1` and `FAR_EL1`, then x0 to x30, two to a line:

```text
ambrosia: exception <index>: ESR <esr> ELR <elr> FAR <far>
ambrosia: x0  <x0>  x1  <x1>
```

The index is the vector's place in the table: 0 to 3 for the current EL on
SP_EL0, 4 to 7 for the current EL on SP_EL1, and 8 to 15 for a lower EL,
each group in the order synchronous, IRQ, FIQ, SError. Ambrosia runs at
EL1 on SP_EL1, so a fault in it or in a module is index 4.
[crates/firmware](../crates/firmware/README.md#reading-an-exception) says
how to find the code at `ELR`.
