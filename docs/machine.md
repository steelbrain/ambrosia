# The machine

Ambrosia runs on QEMU's `vmapple` machine. `vmapple` implements the device
model Virtualization.framework gives macOS guests, with none of Apple's
code. This page describes the machine Ambrosia expects: its memory map, what
Ambrosia reads from it, and what macOS needs from it, with the reason for
each.

Stock QEMU's `vmapple` lacks several of these. For one, it keeps RAM at
0x70000000, where Virtualization.framework's machine has it, and Ambrosia
boots only a machine with RAM at 4 GiB. To run Ambrosia, use a QEMU whose
`vmapple` gives everything in
[What the machine must give](#what-the-machine-must-give).

## Memory map

| Region | Base | Size |
|---|---|---|
| Firmware, the `-bios` image | 0x00100000 | 1 MiB |
| Config block | 0x00400000 | 64 KiB |
| DMA config doorbell | 0x004ffff8 | 8 bytes |
| GIC distributor | 0x10000000 | 64 KiB |
| GIC redistributors | 0x10010000 | 4 MiB |
| GICv2m | 0x1fff0000 | 64 KiB |
| UART, a PL011 | 0x20010000 | 64 KiB |
| RTC, a PL031 | 0x20050000 | 4 KiB |
| GPIO | 0x20060000 | 4 KiB |
| pvpanic | 0x20070000 | 2 bytes |
| Firmware disk interface | 0x30000000 | 2 MiB |
| AES engines | 0x30220000, 0x30230000 | 16 KiB each |
| avp-rtc, the second clock | 0x30240000 | 4 KiB |
| PCIe ECAM | 0x40000000 | 256 MiB |
| PCIe memory window | 0x50000000 | 0xb0000000, up to 4 GiB |
| RAM | 0x100000000 | as `-m` gives |

Ambrosia has this layout built in: the RAM base, the PCIe window, and the
addresses of the devices it uses. Nothing on the machine reports them. The
config block has no field for them and the machine has no `fw_cfg`, so
Ambrosia boots only a machine with this layout.

## What Ambrosia uses

- **The config block**, 64 KiB of RAM the machine fills. Ambrosia reads the
  CPU count at +0x04, the ECID at +0x18, the RAM size at +0x20, a random
  32-bit value the VMM draws once per process at +0x38, and four
  NUL-padded 32-byte strings: the serial number at +0x500, the region at
  +0x520, the model at +0x540 and the SoC's name at +0x584.
- **The firmware disk interface**, which gives the firmware the `aux` and
  `root` drives, the first and second pflash drives. The firmware writes a
  request's address to a drive's command register, at +0x408 for `root`
  and +0x10408 for `aux`, and the machine has filled the buffer and the
  status byte when the store returns. A request names a 32-bit sector of
  512 bytes. The machine refuses one over 128 MiB, and Ambrosia asks for at
  most 16 MiB at a time.
- **The UART**, for its log.
- **avp-rtc's ID register**, at +0x40. When it reads 0xf001, Ambrosia gives
  macOS `avp-rtc` as its clock, and otherwise the PL031, as iBoot decides.
- **The PCIe ECAM and window**, which it hands to a graphics driver module
  and writes into the device tree.

Ambrosia's own accesses to device memory are single-register loads and
stores without writeback, which the CPU reports with a syndrome, so no VMM
has to decode them.

AVPBooter does not boot on this layout: it keeps its stack at 0x70053ff0,
which is in the PCIe window here.

## What the machine must give

### RAM at 4 GiB, and a PCIe window below it

The kernel assigns every PCI BAR itself, from the windows in the device
tree's PCIe node, and places each one below 4 GiB, 64-bit prefetchable BARs
included. A window above 4 GiB goes unused. A BAR is aligned to its size,
so a device with a 1 GiB BAR needs a free, 1 GiB-aligned gigabyte below
4 GiB.

With RAM at 0x70000000, as on Virtualization.framework's machine, the only
window runs from 0x50000000 to 0x6fff0000, 64 KiB short of 512 MiB, and no
such gigabyte is free. So the machine puts RAM at 4 GiB and runs the window
from 0x50000000 up to it. Ambrosia gives the kernel that window, and the
kernel then places a 1 GiB BAR at 0x80000000.

### avp-rtc

Besides the PL031, Virtualization.framework's machine has a second clock
at 0x30240000, which macOS's device tree calls `avp-rtc`. macOS 14 and
later drive it. They keep its offset to calendar time in NVRAM, as
`com.apple.System.rtc-offset`, and timed reads that offset to tell whether
the clock was reset. macOS's PL031 driver keeps no offset. With the PL031
alone, timed takes the clock for reset on every boot and sets it back to
the last write to the guest's disk, by minutes or hours, until it reaches
a time server.

The machine needs the device, with the registers macOS's driver uses, each
64 bits:

- +0x00, a counter of microseconds of QEMU's RTC clock since the Unix
  epoch;
- +0x18 and +0x20, which the driver writes as it starts, and which read
  back what was written;
- +0x30, the pending interrupts, which reads 0;
- +0x38, which the driver writes to clear interrupts;
- +0x40, the ID register, which reads 0xf001. The driver never reads it;
  iBoot and Ambrosia do.

It needs no interrupt line, since nothing on the machine raises one. When
it counts QEMU's RTC clock, `-rtc base=` and `-rtc clock=` move it with the
PL031. The device trees of 12.6 and 13.6 list only the PL031, and those
guests keep it.

Under TCG, reading the ID register of a device the machine lacks is an
external abort, so Ambrosia needs a machine with avp-rtc there.

### The DMA config doorbell

As its platform driver starts, macOS writes the physical address of a
request block, 8 bytes, to 0x004ffff8. The address is a constant in its
code, not one the device tree gives. Virtualization.framework's machine
answers the request. On `vmapple` it can go unanswered, and the driver goes
on without the register it asked for. Under HVF a write to an address with
no device is dropped, but under TCG it is an external abort, and the
kernel panics. So the machine maps an unimplemented device over the 8
bytes.

### virtio-net without its control queue

With a control queue, about one boot in ten of a 26.6.2 or 27.0.1 guest
under HVF never comes up on the network: configd stops before DHCP starts,
though QEMU has completed and signalled the driver's first control command.
The driver seems to miss that completion. With virtio-net offered on this
machine with no control queue, and none of the features that need one, the
network comes up on every boot. Without the queue the device stays
promiscuous.

### No SME under TCG

TCG's `max` CPU offers SME, with streaming vectors of up to 2048 bits. As
a 14.6.1 kernel starts, it creates a zone for each thread's SME state,
sized from the longest vector, and with 256-byte vectors that is 74328
bytes, too large for a zone's element. The kernel panics with
"zone_create: element size too large: 74328". 15.6.1, 26.6.2 and 27.0.1
boot with SME. So the machine turns SME off by default for `max`, and
`-cpu max,sme=on` turns it back on. Under HVF the CPU has whatever SME the
host gives, and an M1, M2 or M3 gives none.

### Secondary CPUs on the boot CPU's keys

XNU on `vmapple` never writes the pointer authentication key registers, so
the kernel and every process run on the keys the firmware left on the boot
CPU. The kernel starts the other CPUs itself, through PSCI `CPU_ON`, which
the VMM handles, not the firmware. A CPU started on zero keys panics on its
first authenticated return, whose pointer the boot CPU signed. So the VMM
starts each `CPU_ON` target on the keys of the CPU that started it, under
HVF and TCG.

### MMIO accesses without a syndrome, under HVF

macOS 13 and later access device registers with load and store pairs,
SIMD&FP registers, and pre- or post-indexed forms such as
`str w9, [x8, #128]!`. The CPU reports those data aborts without a
syndrome (ISV=0), so HVF gives QEMU no decoded access. The VMM must decode
the instruction from guest memory and emulate it. A QEMU that can't stops
with "hvf: unhandled ISV=0 data abort". Under TCG, QEMU emulates every
access already.

### A build without HVF, and a 24 MHz timer under TCG

To run under TCG, the machine must build with TCG as well as HVF, and its
default CPU type must be one TCG has: `max`, which is the host's CPU under
HVF. Under TCG each CPU's generic timer must count at 24 MHz, the
`timebase-frequency` macOS's device tree gives, where TCG's default is
1 GHz.

### No change to the guest at kernel entry

Ambrosia makes the changes macOS needs itself
([boot process](boot-process.md#the-pac-nop-pass)), so the VMM needs no
hook at kernel entry and leaves the guest's kernel alone.

### The rest

- **The GICv2m** sets bit 31 of `V2M_MSI_TYPER`, as macOS expects. From
  14.6.1 on, macOS has a PCIe MSI controller that uses it; 13.6 has none,
  so a PCIe device on 13.6 gets no message-signalled interrupt.
- **HVF's in-kernel vGIC** sits at the machine's GIC bases, for
  `kernel-irqchip=on`.
- **macOS 27's virtio.** macOS 27 never starts a virtio device whose
  capabilities are linked from high offsets to low, as QEMU links them, so
  `vmapple-virtio-blk-pci` and `virtio-net-pci` link them in ascending
  order. `vmapple-virtio-blk-pci` also has subsystem ID 0x1100 and Apple's
  vendor capability: virtio `cfg_type` 9, vendor 0x106b, 14 bytes, with its
  transport-encryption-disabled flag set.
- **The AES engine.** A macOS 26 guest sends it an AES-256-CBC command as
  it mounts its root volume. QEMU needs a crypto library (gnutls, nettle or
  libgcrypt) to answer it. A QEMU built without one fails the command, and
  the guest hangs.
- **USB input.** The guest gets a USB keyboard and `usb-vmapple-tablet`,
  Virtualization.framework's USB pointer (05ac:8106).

Apple's paravirtual devices are not needed. Ambrosia removes the nodes of
the apple-gfx display and the IOSurface mapper from the device tree and has
no driver for them, so a machine without them boots the same.

To build such a QEMU for Ambrosia, configure it with
`--target-list=aarch64-softmmu` and a crypto library, such as
`--enable-gnutls`. On macOS, HVF is built in by default.

## Devices the template lists that the machine lacks

macOS's device tree template describes Virtualization.framework's machine,
which has devices `vmapple` doesn't. Under HVF an access to an address with
no device reads zero, or is dropped if it is a write. Under TCG it is an
external abort, which the kernel doesn't survive.

- **`avp-ctrr`**, a CTRR device at 0x30260000. A 27.0.1 kernel reads it as
  it starts. Ambrosia removes the node, and the kernel then skips its CTRR
  lockdown ("PV CTRR unavailable, skipping lockdown") and makes no access.
- **`avp-sealed-registers`**, at 0x30270000. The guest never accesses it,
  but needs its node: without it, 27.0.1 never answers SSH. Ambrosia keeps
  the node.
- **`gfx`** and **`iosurface-mapper`**, at 0x30200000 and 0x30210000:
  Apple's paravirtual display and IOSurface mapper. Ambrosia removes both.

## The PAC hypercalls

A `vmapple` kernel asks the hypervisor to manage its pointer authentication
keys, with `hvc #0` and function IDs 0xC1000000 to 0xC1000006.
Hypervisor.framework's public API answers the whole range 0xC1000000 to
0xC10000FF itself, without an exit to QEMU, and fails the PAC calls. Only a
private mode implements them, and it needs an entitlement that only
Apple-signed binaries hold, so a VMM could use it only with the host's SIP
off. Under TCG the calls reach QEMU's PSCI code, which answers each as not
supported.

From macOS 14 on, the kernel spins when one of these calls fails.
Ambrosia turns each spin into a `nop`, under HVF and TCG alike, so the VMM
needs to do nothing for them. The kernel and every process then share the
firmware's one set of keys.
