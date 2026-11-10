# Lessons learned

Booting macOS without Apple's firmware took finding out many things that
no document states. Many showed up as a boot that hung with nothing on the
serial port. This page records each one as a fact with its reason, so that
the next person changing the boot, or writing a firmware or a VMM for
macOS guests, doesn't have to find it again. Each names the code that acts
on it.

## Kernel collections and kexts

**The Auxiliary KC is accepted because `vmapple` reports developer mode
force-enabled.** On a Mac, a third-party kext in the Auxiliary KC needs
Reduced Security. On `vmapple` the kernel's code-signing checks run with
developer mode force-enabled, so they accept the Auxiliary KC's kexts with
no cdhash in any trust cache, and SIP stays on. That is what makes
injection possible with nothing done in the guest.

**Merge a collection's segments by name, or the boot hangs.** The kernel's
fileset loader resolves each kext's segments against the collection's
top-level segments by name. With one `__TEXT_EXEC` per kext, two segments
share the name, the loader resolves a kext's segment to the wrong one, and
the boot hangs. A collection must have exactly one segment of each name,
holding that segment of every kext, as any kernel collection does
(`COLL_ORDER` in `auxkc.rs`).

**Resolve binds against every member of the Boot KC.** A kext imports
standard kernel programming interfaces that the kernel re-exports from
other members of the collection, such as `IOCatalogue::addDrivers` from
`com.apple.kpi.iokit`, and a graphics kext imports from `IOPCIFamily` and
`IOGraphicsFamily`. Resolving against `com.apple.kernel`'s own symbols
alone fails those binds. The kernel's own kext linker binds such
flat-namespace imports against the whole collection, so Ambrosia takes
the union of every member's external symbols, with the kernel's own
definition winning a duplicate (`Header::all_symbols` in `macho.rs`).

**Leave the Auxiliary KC's pointers unsigned.** The kernel's deferred
loader walks the collection's chained fixups, adds the base of the
collection each word names, and signs every authenticated slot itself.
With the pointers signed in advance, the loader authenticated pointers it
had never signed, and the kext's start trapped (FEAT_FPAC). So the slots are written as raw `DYLD_CHAINED_PTR_64_KERNEL_CACHE`
words (`build_chain` in `auxkc.rs`).

**Link the Auxiliary KC at zero.** The kernel adds the collection's load
address, which it takes from `/chosen/memory-map`'s `AuxKC-mach_header`,
to every structural address in it. The prelink dictionary's load addresses
and `_PrelinkKmodInfo` are offsets the loader adds the load address to as
well, so an absolute value there is added twice and wraps.

**Put the Auxiliary KC below the Boot KC.** The kernel's VM bootstrap
panics unless the Auxiliary KC's lowest segment lies below the Boot KC's
(`prepare` in `boot.rs`).

**Reserve each segment's full virtual size.** A kext's `__DATA` may end in
a zero-fill tail, its bss. The collection must reserve the segment's whole
virtual size, not just its bytes in the file, both where it lays the
segment out and in the room it keeps for the collection below the Boot KC.
Short of that, a pointer into the bss lands outside the kext, and the
built collection overruns its room into the trust caches and the Boot KC
above it, and the kernel faults at entry (`segments_size` in `auxkc.rs`).

**Mark a placed kext's header `MH_DYLIB_IN_CACHE`.** The flag tells the
kernel the Mach-O is already in a collection, so it slides the header's
segment addresses in place. Without it the kernel relocates the kext as a
standalone image, the addresses come out as zero, and the check that the
kext's start function lies in its code fails. The header's own symbol and
link-edit tables must be emptied too, or the kernel walks them through the
kext's unplaced `__LINKEDIT` and takes a data abort (`rewrite_sub_header`
in `place.rs`).

**Re-encode `adrp` after moving segments.** Merging segments moves a
kext's code and data to new distances from each other, so every
page-relative `adrp` the compiler computed is off. Each segment lands on a
16 KiB boundary, so only the page immediate changes, and the `add` or
`ldr` that carries the low 12 bits stays right wherever the compiler put it
(`relink_adrp` in `place.rs`).

**Two UUIDs must match.** The kernel reads the Auxiliary KC's identity from
its `LC_UUID` and, as it registers the collection, from the prelink info's
`_PrelinkKCID`, and rejects the collection if they differ. `_BootKCID` must
be the running Boot KC's `LC_UUID`.

**A vtable slot's discriminator comes from its first declaration.** When a
C++ class declares an inherited method pure again, its own vtable signs
that slot with a discriminator of its own, while callers use the one from
the method's first declaration. Code that builds a vtable from the
redeclaring class's values faults the first time the slot is called. So
discriminators are taken from the most-base class that has the slot
([the handover](handover.md#vtable-discriminators)).

## Memory layout

**`virtBase - physBase` must be a multiple of 32 MiB.** The kernel maps
itself in 32 MiB blocks and builds its mapping as if this holds. When it
doesn't, the kernel's first exception loops in a vector that isn't there.

**The trust caches go below the kernel collection, the device tree above
it.** 26.6.2 and later panic on trust caches above the collection ("TrustCache
region is in an unexpected place"). Early in its start, 14.6.1 maps
nothing below the collection, and 26.6.2 nothing below the collection's
32 MiB block, so a device tree placed lower can't be read and the kernel's
first read of it faults (`prepare` in `boot.rs`).

**Clean the caches before entering the kernel.** The kernel starts with
its caches off. Any dirty line Ambrosia leaves can be written back later
over memory the kernel wrote since, so Ambrosia cleans and invalidates
everything it wrote, and its heap, to the point of coherency first
(`main.rs`).

**Zero the bss by hand.** The machine reloads the image's bytes into the
firmware region on reset, but not the rest of the region, so the bss
holds what the last boot left (`start.s`).

**Turn the MMU on before doing real work.** With the MMU off every access
is treated as one to device memory, where an unaligned access faults, and
compiled code makes unaligned accesses. So Ambrosia maps RAM as normal
memory with an identity map as it starts (`mmu.rs`).

## Pointer authentication

**XNU on `vmapple` never sets its own keys.** The keys the firmware leaves
are the kernel's and every process's for the guest's life. The kernel
starts the other CPUs through PSCI, which the VMM handles, so the VMM must
start each one on the boot CPU's keys, or it panics on its first
authenticated return (`keys.rs`).

**From macOS 14 the kernel spins on a refused PAC hypercall.** A
`vmapple` kernel asks the hypervisor to manage its keys with `hvc #0`.
Hypervisor.framework's public API answers that whole range of function IDs
itself and fails the calls, and only a private mode, which needs the host's
SIP off, implements them. From macOS 14 on, the kernel follows each call
with `cbnz x0, .`, and spins forever. Turning those 19 branches into `nop`s
lets it run on the firmware's keys, at the cost of one key set for the
kernel and every process (`pac.rs`;
[the PAC spins](replacing-iboot.md#the-pac-spins)).

## The machine

**HVF gives no decode for an MMIO access without a syndrome.** macOS 13
and later access device registers with load and store pairs, SIMD
registers, and forms with writeback. The CPU reports those data aborts
with ISV=0, so HVF hands the VMM no decoded access, and the VMM must decode
the instruction from guest memory. Ambrosia's own device accesses are
plain loads and stores, which carry a syndrome, so it needs no such
decoding.

**RAM had to move to 4 GiB for a 1 GiB BAR to fit.** The kernel places
every BAR below 4 GiB, 64-bit prefetchable ones included: its PCI
configurator clears the flag that would lift a 64-bit BAR's limit before
its first pass, and a window above 4 GiB goes unused. A BAR is aligned to
its size, and with RAM at 0x70000000 no 1 GiB-aligned gigabyte below 4 GiB
was free. With RAM at 4 GiB and the PCIe window up to it, the kernel puts a
1 GiB BAR at 0x80000000 (`pcie_window` in `tree.rs`).

**Don't stretch `/arm-io`'s range; add one.** A device's memory is its BAR
resolved through the PCIe node's `ranges` and then `/arm-io`'s, so a BAR
outside `/arm-io`'s range is assigned but can't be mapped by its driver.
With the template's range stretched to 4 GiB, 27.0.1 stops before its
first console line. A second range, from 0x80000000 to 4 GiB, works on
every tested build (`pcie_window` in `tree.rs`).

**A device node can matter with no device behind it.** Nothing accesses
`avp-sealed-registers`, but without its node 27.0.1 never answers SSH. So
Ambrosia keeps it, while it removes the nodes of other devices the machine
lacks (`arm_io` in `tree.rs`).

**macOS 14 and later need avp-rtc to keep calendar time.** macOS keeps the
offset from avp-rtc's counter to calendar time in NVRAM. Its PL031 driver
keeps none, so with the PL031 alone timed takes the clock for reset on
every boot and sets it back to the disk's last write. A guest that has
never set its clock under avp-rtc has no offset either, so Ambrosia adds
the driver's value for zero when NVRAM has none (`prepare` in `boot.rs`).

**A booter doesn't need `/filesystems` to boot macOS 15 and later.** Booted
by iBoot on `vmapple`, macOS 15 and later panic in APFS on the unencrypted
Data volume unless the tree has `/filesystems` with `DoNotUseSEP`. Booted
by Ambrosia, they mount it unencrypted without one. Which part of iBoot's
tree makes the difference is not known.

**A display node must be in the tree from the start.** macOS's
IOMobileFramebuffer framework counts the `display-subsystem` nodes under
`/arm-io` once per process, the first time the process asks for a display,
and a process that counts none never looks for a framebuffer driver.
WindowServer asks before any Auxiliary-KC kext has loaded, so a node a kext
adds comes too late. That is what `Graphics` > `DisplayNode` is for
(`display_node` in `tree.rs`).

**The kernel's console stays where the boot arguments put it.** It draws
at the framebuffer address it was given, even if the kernel later moves
the BAR that held it, and QEMU never maps a 32-bit BAR that reaches
0xffffffff, which the kernel then moves. So a driver module places its
BARs where the kernel keeps them, inside the window and short of
0xffffffff.

**virtio-net's control queue loses the network on some boots.** With it,
about one boot in ten of 26.6.2 or 27.0.1 under HVF never starts DHCP,
though the VMM completed the driver's first control command. Offered with
no control queue, the device brings the network up on every boot.

**macOS 27 orders virtio capabilities.** It never starts a virtio device
whose capabilities are linked from high offsets to low, as QEMU links
them.

## TCG

**What a TCG machine needs beyond HVF.** macOS boots under TCG on QEMU's
`max` CPU, with Arm's ID registers, and touches none of Apple's
implementation-defined registers that TCG lacks. It needs:

- a generic timer at 24 MHz, the frequency its device tree gives, where
  TCG's default is 1 GHz;
- nothing that reads or writes an address with no device: under HVF such
  an access reads zero or is dropped, but under TCG it is an external
  abort. macOS makes two. The kernel reads `avp-ctrr` as it starts, so
  Ambrosia removes that node, and the kernel then skips the read. The
  platform driver writes a request to a DMA config doorbell at a constant
  address, so the machine needs a device there;
- no SME, for 14.6.1: its kernel sizes a zone for each thread's SME state
  from the longest streaming vector, and with TCG's 2048-bit vectors that
  zone's element is too large, so the kernel panics;
- the PAC `nop`s, as under HVF: the hypercalls reach QEMU's PSCI code,
  which answers them as not supported.

Any of TCG's pointer authentication algorithms works, and the default,
implementation-defined one is the fastest.

## Debugging

**Without boot arguments, a failing kernel shows nothing.** With no
`serial=3`, the kernel writes nothing to the UART, so a panic shows only
as a reset: Ambrosia's log starts again. An early fault shows as the boot
CPU spinning. The cause is in the boot CPU's registers and its stack's
saved exception state, or, simpler, in the kernel's own log once
`BootArgs` asks for it
([reading the boot log](../how-to/troubleshoot-a-boot.md)).

**AVPBooter can't boot the machine Ambrosia needs.** AVPBooter keeps its
stack at 0x70053ff0, which is RAM on Virtualization.framework's machine and
the PCIe window once RAM is at 4 GiB, so it faults on its first store
there.
