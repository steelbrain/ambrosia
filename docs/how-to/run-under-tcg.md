# Running under TCG

Under TCG, QEMU emulates the guest's CPUs, so a guest boots on any host,
x86-64 Linux included. It is slower than HVF: with 4 vCPUs on an 8-core
x86-64 host, a guest answers SSH one to two and a half minutes after QEMU
starts.

## What you need

- A QEMU built for `aarch64-softmmu` with TCG and a crypto library such as
  gnutls, whose `vmapple` machine gives what
  [the machine](../machine.md#what-the-machine-must-give) lists. Under TCG
  these matter most:
  - the machine builds without HVF, and its default CPU is `max`;
  - each CPU's generic timer counts at 24 MHz, as macOS's device tree says;
  - `max` has SME off by default, or 14.6.1's kernel panics as it starts;
  - avp-rtc is at 0x30240000, since Ambrosia reads its ID register, and an
    access to a device the machine lacks is an external abort under TCG;
  - an unimplemented device covers the DMA config doorbell at 0x004ffff8,
    which macOS writes as it starts;
  - each secondary CPU starts on the boot CPU's pointer authentication
    keys.
- The image, `target/ambrosia.bin`. The build is reproducible, so an image
  built on the Mac or on the TCG host is the same file.
- A guest installed on a Mac ([installing a guest](install-a-guest.md)),
  copied to the host, and its ECID.

Guests are installed under Virtualization.framework, so installing one
needs an Apple silicon Mac even when it runs under TCG elsewhere.

## Copy the guest

Copy `disk.img` and `aux.img`, or `aux.img.trimmed`, to the host. The disk
image is sparse, so keep it sparse in the copy, for example with rsync:

```sh
rsync --sparse disk.img aux.img host:vm/
```

Boot a clone of the copy, so the copy stays as it was: `cp --reflink=always`
makes one on Btrfs or XFS without copying the data.

## Boot

```sh
scripts/run-guest.sh -t -e "$ECID" -f target/ambrosia.bin \
    -q /path/to/qemu-system-aarch64 path/to/vm
```

`-t` runs QEMU with `-accel tcg,thread=multi`, which runs each vCPU on a
host thread of its own. Nothing else on the command line changes from HVF.
The machine picks the CPU, `max`, and its timer.

QEMU takes more host memory than the guest has: up to about 10.5 GiB for
an 8 GiB guest.

## What differs from HVF

- **The CPU.** The guest runs on TCG's `max` CPU, which reports Arm's ID
  registers rather than Apple's. macOS boots on it, and touches none of
  Apple's implementation-defined registers that TCG lacks.
- **Pointer authentication.** The kernel's PAC hypercalls reach QEMU's PSCI
  code, which answers each as not supported, so Ambrosia's `nop`s are
  needed here as under HVF. Any of TCG's pointer authentication algorithms
  works. The default, the implementation-defined one, is the fastest:
  `-cpu max,pauth-qarma3=on` and `-cpu max,pauth-qarma5=on` take two to
  three times as long to reach launchd.
- **Device space.** An access to an address with no device is an external
  abort, where HVF reads zero. Ambrosia removes the one device node the
  kernel would read without a device behind it, `avp-ctrr`
  ([devices the template lists](../machine.md#devices-the-template-lists-that-the-machine-lacks)).

## Problems you may meet

- Ambrosia's log stops with an `exception` line whose `FAR` is
  `0x30240040`: the machine has no avp-rtc, and Ambrosia's read of its ID
  register aborted.
- A 14.6.1 guest never answers SSH, and with `serial=3 -v` boot arguments
  the kernel panics with "zone_create: element size too large: 74328": the
  CPU has SME. Turn it off with `-- -cpu max,sme=off`.
- The guest never answers SSH and, with `serial=3 -v`, the kernel panics
  with "Undefined kernel instruction": the machine has nothing at the DMA
  config doorbell, and the kernel's write there aborted.

[Reading the boot log](troubleshoot-a-boot.md) covers the rest, and shows
how to put the kernel's log on the serial port.
