# How Ambrosia replaces iBoot

On Virtualization.framework's machine two Apple programs run before macOS.
AVPBooter is the firmware: it starts from the machine's firmware region and
hands over to iBoot. iBoot reads the boot objects from the guest's Preboot
volume, checks their signatures against the boot manifest, lays them out in
RAM, fills in the device tree template, writes the kernel's boot
arguments, and jumps into the kernel.

Ambrosia takes the place of both. It does iBoot's work itself, from the
same inputs, and checks no signatures, so no Apple code runs before the
kernel. This page explains how each part of that work is done and why.
[The boot process](../boot-process.md) lists the steps in the order the
firmware takes them, with each value it writes.

## What the kernel expects from its booter

The kernel finds everything through two things iBoot gives it: the boot
arguments, whose physical address is in x0 at entry, and the device tree,
which the boot arguments point to. Between them they say:

- where RAM is, which part of it is the kernel's, and how it is mapped
  (`physBase`, `virtBase`, `memSize`, `topOfKernelData`);
- where each boot object is: the kernel collection and its parts, the trust
  caches, the device tree itself, and the carveouts at the top of RAM
  (`/chosen/memory-map` and `/chosen/carveout-memory-map`);
- what the machine is: its CPUs, its devices, its PCIe window, its
  identity;
- what the boot is: the install it came from, its security state, its
  random seeds, and NVRAM.

Ambrosia writes each of these as iBoot does. Where a value depends only on
the machine and not on anything iBoot reads, Ambrosia writes iBoot's value.
Where iBoot writes something the kernel doesn't need, such as its own
breadcrumbs (`iboot-handoff`, `preoslog`), Ambrosia leaves it out, and
macOS boots without it.

## Loading the kernel collection

iBoot finds the kernel collection through the Preboot volume of the main
APFS container: the volume group of the System volume names the install,
and `/<group>/boot/active` names the directory of its boot objects.
Ambrosia reads the same path with its own read-only GPT and APFS readers,
through the machine's firmware disk interface.

The collection is an IM4P whose payload is LZFSE-compressed, or stored as
it is on 12.6. Unpacked, it is an arm64e `MH_FILESET` Mach-O whose file
offsets equal its virtual offsets, so Ambrosia unpacks it straight to where
it will run, as one block. The kernel applies the collection's chained
fixups itself once it runs, so Ambrosia doesn't.

iBoot would refuse a collection that doesn't match its signature. Ambrosia
checks no signatures, and changes the collection in only two ways: the
PAC spin `nop`s ([below](#the-pac-spins)) and the patches a configuration
asks for. Kexts it is given go in a separate collection.

**Where it goes.** The kernel maps itself in 32 MiB blocks, so
`virtBase - physBase` must be a multiple of 32 MiB. Ambrosia puts the
collection at a random 16 KiB page in RAM's first 32 MiB and picks a slide
from 32 MiB to 512 MiB that keeps the collection's virtual address at the
same offset in its block as its physical one. Around it:

- the trust caches go right below the collection, since 26.6.2 and later
  panic on trust caches above it;
- the device tree goes above it, since early in its start 14.6.1 maps
  nothing below the collection and 26.6.2 nothing below its 32 MiB block,
  so a tree placed lower can't be read;
- the Auxiliary KC, when there are kexts, goes below the trust caches.

## The Auxiliary kernel collection

On a Mac, a third-party kext is linked into the Auxiliary kernel
collection, which the kernel loads after the Boot KC, once early user
space runs. On real hardware that needs Reduced Security. Ambrosia builds
an Auxiliary KC in memory holding every kext the configuration names, and
gives it to the kernel the way a booter gives one
([`auxkc.rs`](../../crates/ambrosia/src/auxkc.rs),
[`place.rs`](../../crates/ambrosia/src/place.rs)).

Three things let the kernel take it with SIP on and nothing done in the
guest:

- The Boot KC stays as Apple built it, but for the patches above, so no
  signature has to be forged.
- On `vmapple` the kernel runs with developer mode force-enabled, as the
  machine reports it, so it accepts the Auxiliary KC's kexts with no cdhash
  in any trust cache.
- The Auxiliary KC's pointers are left as chained fixups, which the
  kernel's loader rebases and signs itself, so Ambrosia signs nothing.

**The collection.** It is an `MH_FILESET` linked at base zero. Every
structural address in it, those of its segments and sections and each
kext's header, is an offset from its start. The kernel takes the
collection's load address from `/chosen/memory-map`'s `AuxKC-mach_header`
and adds it to each.

Its segments, in order:

- `__TEXT`, whose first page holds the collection's header and load
  commands, then every kext's `__TEXT`;
- `__TEXT_EXEC`, every kext's code;
- `__DATA_CONST`, every kext's `__DATA_CONST` and `__OBJC_CONST`;
- `__DATA`, every kext's `__DATA`;
- `__LINKEDIT`, one page holding the collection's chained fixups and an
  empty symbol table;
- `__PRELINK_INFO`, the prelink dictionary.

The kexts' segments are merged by name, as in any kernel collection: there
is one segment of each name, holding that segment of every kext in turn.
The kernel maps each top-level segment by its name, code executable and
data writable, and resolves a kext's own segments against the top-level
ones by name, so there must be exactly one of each. Each kext's segment
takes its full virtual size, so its zero-fill tail, its bss, is real,
zeroed memory.

The load commands are a segment command for each segment, an
`LC_FILESET_ENTRY` for each kext that names it by its bundle identifier and
points at its header, `LC_DYLD_CHAINED_FIXUPS`, `LC_SYMTAB` and `LC_UUID`.

**Each kext.** Ambrosia copies the kext's segments to their places, then:

- re-encodes the page immediates of the kext's `adrp` instructions, since
  merging moved its segments to new distances from each other. Every
  segment starts on a 16 KiB boundary, so the low 12 bits of each address,
  which the companion `add` or `ldr` carries, stay right;
- rewrites the kext's own header to the placed addresses and marks it
  `MH_DYLIB_IN_CACHE`, as a kext in a collection is marked, so the kernel
  slides it in place instead of relocating it as a standalone image. Its
  own symbol and link-edit tables are emptied, since the kernel would
  otherwise walk them through the kext's unplaced `__LINKEDIT`;
- writes its `kmod_info`;
- adds its prelink dictionary: the keys of its Info.plist, with
  `IOKitPersonalities` and `OSBundleLibraries` among them, and its load
  address, size and `kmod_info` address, as offsets from the collection's
  start. The kernel adds the load address to these itself.

**The fixups.** Every kext's pointers go in one chained-fixups table, in
`DYLD_CHAINED_PTR_64_KERNEL_CACHE` format, which is what kernel collections
use. Each pointer slot holds a word whose target is an offset from one of
the kernel's collection bases:

- a pointer into the kext, or another placed kext, has `cacheLevel` 3, this
  collection, and the target's offset in it;
- a bind to a symbol of the Boot KC has `cacheLevel` 0, and the symbol's
  offset from the Boot KC's base.

A signed pointer keeps its key, diversity and address-diversity bit, and
the kernel signs it as it rebases it. An imported symbol resolves first
against the external symbols of every member of the Boot KC, with the
kernel's own definition winning a duplicate, and then against the symbols
the other placed kexts export. That is how a plugin kext binds to the base
kext it depends on in the same configuration.

**The prelink info.** `_BootKCID` is the Boot KC's `LC_UUID`, which the
kernel checks against the Boot KC it is running. `_PrelinkKCID` is the
collection's own UUID, the same as its `LC_UUID`, which the kernel checks
when it registers the collection. `_PrelinkInfoDictionary` holds the kexts'
dictionaries.

**Where it goes.** The kernel's VM bootstrap requires the Auxiliary KC's
lowest segment to lie below the Boot KC's, so the collection goes below
the trust caches, in the same 32 MiB block as the Boot KC. Ambrosia
reserves its size before it lays out anything else, from the same segment
sizes it builds the collection with. `/chosen/memory-map` gets `AuxKC`, the
whole collection, and `AuxKC-mach_header`, its base, both the same range,
as the Boot KC's entries are written.

Ambrosia also records what it placed under `/chosen/ambrosia`, for code
that runs in the kernel to find ([the handover](handover.md)).

## The device tree

The guest's device tree template, `devicetree.img4` in its boot objects,
describes Virtualization.framework's machine with blanks for the booter to
fill. Ambrosia fills it as iBoot does
([`tree.rs`](../../crates/ambrosia/src/tree.rs)):

- **Directives.** A property whose length has its top bit set asks the
  booter for a value. Ambrosia fills the serial number, model and region
  from the config block, and zeroes, and drops any it has no value for, as
  iBoot does.
- **The security state.** `/chosen` gets the values of a Full Security
  boot, such as `secure-boot`, `effective-security-mode-ap` and
  `effective-production-status-ap`, each 1, and `/chosen/asmb` the
  LocalPolicy's properties. SIP is on, and nothing tells the kernel
  otherwise.
- **The boot.** The install's identity and root hash, the boot manifest's
  properties and hashes, the random seeds, NVRAM, and the memory maps.
- **The machine.** The CPUs the machine has, the devices it has, and its
  PCIe window.

Two parts differ from iBoot's tree on purpose.

**Only the devices the machine has.** The template lists devices `vmapple`
lacks. Ambrosia removes `gfx` and `iosurface-mapper`, Apple's paravirtual
display and IOSurface mapper, which it doesn't offer; `avp-ctrr`, a CTRR
device the kernel reads as it starts; and one of the two clocks, as iBoot
chooses. It keeps `avp-sealed-registers`, which nothing accesses but
without which 27.0.1 never answers SSH.

**The PCIe window.** The template's PCIe `ranges` give
Virtualization.framework's window, 64 KiB short of 512 MiB. Ambrosia
writes the machine's, from 0x50000000 up to 4 GiB, so the kernel can place
a 1 GiB BAR. A device's memory is its BAR's address resolved through the
PCIe node's `ranges`, then `/arm-io`'s, so `/arm-io` needs a range that
covers the window too. Its template range must stay as it is, since
27.0.1 stops before its first console line with that range stretched, so
Ambrosia adds a second range from 0x80000000 to 4 GiB.

One thing iBoot's tree has is not needed. Booted by iBoot, macOS 15 and
later panic in APFS on the unencrypted Data volume unless the tree has a
`/filesystems` node with `DoNotUseSEP`. Booted by Ambrosia, APFS mounts
each volume unencrypted without the node, so Ambrosia adds none.

## The boot arguments

XNU's `boot_args`, revision 3, version 2: the memory layout, the video
fields, the device tree's address and length, and the command line from
the configuration's `BootArgs`
([`bootargs.rs`](../../crates/ambrosia/src/bootargs.rs)). The video fields
give the framebuffer iBoot carves out of RAM, 640 by 1136 at 32 bits, or,
when a graphics driver module set up a display, that display's
framebuffer, which the kernel's console then draws on.
[The boot process](../boot-process.md#the-boot-arguments) has every field.

## The PAC spins

A `vmapple` kernel asks the hypervisor to manage its pointer
authentication keys, with `hvc #0` and function IDs from 0xC1000000:
SET_INITIAL_STATE as each CPU starts, GET_DEFAULT_KEYS, and calls that set
a process's B keys and its EL0 diversifier on context switches.
Hypervisor.framework's public API answers the whole range 0xC1000000 to
0xC10000FF itself, without an exit to the VMM, and fails every PAC call.
Only a private mode implements them, and it needs an entitlement only
Apple-signed binaries hold. Under TCG the calls reach QEMU's PSCI code,
which answers each as not supported.

macOS 13's kernel doesn't spin on a failure. From macOS 14 on, the kernel spins
when one fails: `hvc #0` followed by `cbnz x0, .`, a branch to itself.
14.6.1 to 27.0.1 have 19 such sites: in every CPU's start code, in
GET_DEFAULT_KEYS, and on context switches. Ambrosia turns each `cbnz x0, .`
that directly follows an `hvc #0` in the collection's executable segments
into a `nop` ([`pac.rs`](../../crates/ambrosia/src/pac.rs)). On 12.6 and
13.6 the pass finds nothing and changes nothing.

**The cost.** The kernel then runs on the keys the firmware set, and so
does every process: one set of keys for the guest's life, with no
per-process B keys and no EL0 diversifier. A pointer a process signs with
a B key would authenticate in any other process, given the same modifier.

**The other way.** The VMM could answer the calls and keep per-process
keys. Under TCG they reach QEMU, which could. Under HVF only an `smc #0`
with a PAC function ID exits to the VMM, so the kernel's 19 `hvc #0` would
have to become `smc #0`, still a change to the kernel. Every call would
then be a VM exit, and a boot under TCG makes about 300,000 PAC calls in
its first 10 seconds. And without Apple's per-exception-level keys, which
the public HVF API doesn't give, the kernel would still share its keys
with user space. So Ambrosia takes the `nop`s.

## The keys

XNU on `vmapple` never writes the pointer authentication key registers.
The keys the firmware leaves on the boot CPU are the ones the kernel and
every process run on until the guest shuts down. AVPBooter writes one
32-bit random value, repeated, into all of them. Ambrosia derives five
distinct 128-bit keys, IA, IB, DA, DB and GA, with SHA-384 from the boot's
seed, which hashes the machine's random value with the virtual counter, so
they also change between boots of one QEMU process
([`keys.rs`](../../crates/firmware/src/keys.rs)).

The kernel starts the other CPUs through PSCI `CPU_ON`, which the VMM
handles, not the firmware. So the VMM must start each one on the keys of
the CPU that started it; a CPU on zero keys panics on its first
authenticated return.

## Entering the kernel

iBoot enters the kernel on the boot CPU alone:

- at EL1, on SP_EL1, with D, A, I and F masked;
- with the MMU and caches off, `TCR_EL1`, `TTBR0_EL1`, `TTBR1_EL1` and
  `VBAR_EL1` zero, and FP enabled;
- with x0 at the boot arguments' physical address, the PC and x30 at the
  collection's entry point, and every other general register zero.

Ambrosia runs with the MMU and caches on, so before it turns them off it
cleans and invalidates to the point of coherency every range it wrote for
the kernel and the part of its heap it used. No dirty cache line is then
left to be written back over memory the kernel writes with its caches off.
`enter_kernel`, in [`start.s`](../../crates/firmware/src/start.s), turns
the MMU and caches off, invalidates the instruction cache and the TLB,
zeroes `VBAR_EL1`, `TTBR0_EL1` and `TCR_EL1`, sets the registers and
branches to the entry point.

Ambrosia never writes `TTBR1_EL1`, which QEMU resets to zero.

## What Ambrosia leaves out

- **Signature checks.** Nothing is checked against its manifest. The
  manifests are still placed in memory, and their properties and hashes
  written into the device tree, as iBoot does.
- **recoveryOS.** Ambrosia boots the installed system. It skips the
  recoveryOS container and the LocalPolicy recoveryOS boots with, and
  removes `/osenvironments`, the overlays for a recoveryOS boot.
- **The Secure Enclave.** The machine has none. The SEPFW entry of the
  memory map points at an empty page.
- **Apple's paravirtual devices.** Ambrosia has no driver, configuration
  key or handover field for them, and removes their nodes.
