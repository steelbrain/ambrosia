# Ambrosia

Ambrosia is boot firmware for arm64 macOS guests on QEMU's `vmapple`
machine. QEMU loads it with `-bios`, in place of Apple's AVPBooter. It
reads macOS from the guest's disk, prepares the kernel as Apple's iBoot
would, and starts it with System Integrity Protection enabled. Neither
AVPBooter nor iBoot runs. A configuration can add kexts, which the kernel
then loads and starts with SIP still on, as OpenCore adds kexts to x86-64
macOS.

## What works

Plain guests of these macOS builds boot with SIP enabled and no boot
arguments, checked by `csrutil status` over SSH:

| macOS | Under HVF (Apple silicon) | Under TCG (any host) |
|---|---|---|
| 12.6 (21G115) | yes | not tested |
| 13.6 (22G120) | yes | yes |
| 14.6.1 (23G93) | yes | yes |
| 15.6.1 (24G90) | yes | yes |
| 26.6.2 (25G83) | yes | yes |
| 27.0.1 (26A434) | yes | yes |
| 27.2.1 (26B131) | yes | yes |

Under HVF a guest answers SSH about 10 to 25 seconds after QEMU starts.
Under TCG, with 4 vCPUs on an 8-core x86-64 host, it takes one to two and
a half minutes.

Kexts named in the configuration are placed in an Auxiliary kernel
collection that Ambrosia builds in memory, and the kernel starts them with
SIP enabled. Nothing is done in the guest for it: it needs neither Reduced
Security nor kext approval. This has been tested on 13.6 to 27.2.1.

What it doesn't do:

- **Show a desktop on its own.** macOS draws its desktop only with a Metal
  device it has a driver for. Without one, a guest runs its services and
  answers SSH, but WindowServer waits for a Metal device and the login
  session never starts.
- **Install macOS or boot recoveryOS.** Ambrosia boots an installed macOS.
  Install the guest with Virtualization.framework first.

## What you need

- **A QEMU whose `vmapple` machine has the changes
  [docs/machine.md](docs/machine.md) lists.** Stock QEMU lacks them: among
  other things its `vmapple` keeps RAM at 0x70000000, and Ambrosia needs
  RAM at 4 GiB with the PCIe window below it. This repository doesn't
  include such a QEMU; the machine page says what each change is and why
  macOS needs it.
- **A macOS guest installed by Virtualization.framework**, which needs an
  Apple silicon Mac: its `disk.img`, its `aux.img`, and the ECID from its
  machine identifier
  ([installing a guest](docs/how-to/install-a-guest.md)).
- **A host to run it on**: an Apple silicon Mac under HVF, or any host
  under TCG.
- **To build Ambrosia**: Rust through rustup, which installs the toolchain
  `rust-toolchain.toml` pins, and `make`.

## Quick start

Build the image, or download a release's `ambrosia-<version>.zip`, which
holds `ambrosia.bin` and its `SHA256SUMS` to check it against:

```sh
make                       # writes target/ambrosia.bin
```

Boot a guest with it:

```sh
scripts/run-guest.sh -e 0x1234567890abcdef \
    -q /path/to/qemu-system-aarch64 /path/to/vm
```

`/path/to/vm` holds the guest's `disk.img` and `aux.img`, and `-e` gives
its ECID. The script boots `target/ambrosia.bin` unless `-f` names another
image, runs under HVF unless `-t` asks for TCG, prints the serial log to
the terminal, and forwards `127.0.0.1:10022` to the guest's SSH port.
Ambrosia's log ends with `entering the kernel at ...`. Then wait for SSH:

```sh
ssh -p 10022 user@127.0.0.1 csrutil status
```

`scripts/run-guest.sh -h` lists the options, and
[running a guest](docs/running.md) explains the command line it builds.

## Configuration, kexts and bundles

A configuration bundle is a cpio archive holding `config.plist` and the
files it names, which QEMU loads into guest RAM at `0x120000000`.
`config.plist` can add kexts, patch kernel collection entries, set boot
arguments and name a graphics driver module. `ambrosia-bundle` builds a
bundle and checks it with the firmware's own parsers:

```sh
ambrosia-bundle create --config config.plist --kext Hello.kext \
    --output bundle.cpio
scripts/run-guest.sh -e "$ECID" -b bundle.cpio /path/to/vm
```

- [Configuration](docs/configuration.md) documents every key.
- [Building a bundle](docs/how-to/build-a-bundle.md) covers
  `ambrosia-bundle`.
- [Adding a kext](docs/how-to/add-a-kext.md) builds and injects
  [`examples/hello-kext`](examples/hello-kext/README.md), a minimal kext.
- [The handover](docs/internals/handover.md) is how code in a placed kext
  finds where Ambrosia put it.
- [Graphics drivers](docs/graphics-drivers.md) is the interface for a
  module that shows the boot on a display device, with
  `drivers/bochs-display` as its example: `make bochs-display-bundle`
  builds a bundle that shows the boot on QEMU's `bochs-display`.

## How it works

At reset, Ambrosia:

1. reads its configuration bundle from RAM, if QEMU loaded one, and starts
   the graphics driver module it names;
2. reads the guest's disks through the machine's firmware disk interface:
   NVRAM from the aux disk, and from the root disk the GPT, the APFS
   container and the boot objects on its Preboot volume;
3. sets the pointer authentication keys the kernel and every process will
   use;
4. unpacks the kernel collection, turns into `nop`s the loops where macOS 14
   and later spin when the hypervisor refuses their pointer authentication
   calls, as HVF's public API and TCG both do, and applies the
   configuration's patches;
5. builds an Auxiliary kernel collection for the configuration's kexts;
6. writes the device tree from the guest's own template, and the kernel's
   boot arguments;
7. enters the kernel.

It logs each step to the machine's UART.
[The boot process](docs/boot-process.md) describes each step, and
[how Ambrosia replaces iBoot](docs/internals/replacing-iboot.md) why it
works the way it does.

## Repository layout

| Path | What it holds |
|---|---|
| `crates/ambrosia` | The boot, apart from the machine: disks, APFS, the boot objects, the kernel collection, the Auxiliary KC, the device tree. It builds and tests on the host. |
| `crates/firmware` | The firmware image: start-up, MMU, UART, the disk interface, and the call into `crates/ambrosia`. |
| `crates/ambrosia-bundle` | `ambrosia-bundle`, the host tool that builds and checks configuration bundles. |
| `drivers/bochs-display` | The example graphics driver module. |
| `examples/hello-kext` | A minimal kext to inject, with its `config.plist`. |
| `scripts/run-guest.sh` | Boots a guest with Ambrosia. |
| `scripts/internal` | Maintainer tools: a release's license notices and its zip. |
| `docs` | How to build, run, configure and extend Ambrosia, and how it works. |

## Building and testing

```sh
make                  # the image, target/ambrosia.bin
make test             # host tests, including loading the example module
make lint             # rustfmt and clippy; warnings fail
make ci               # everything CI runs, on a clean checkout
make help             # every target
```

The build maps machine-specific paths to fixed names and pins the
toolchain, so a commit builds the same bytes on any host. `make release`
writes a release into `dist/`: the image, its checksum, the license notices
of the crates it links in, and the commit it came from.
`make verify-release` rebuilds that commit and compares. CI runs `make ci`
on Linux and on macOS and checks that both build the same release.
[Building](docs/building.md) has the details, and
[CONTRIBUTING.md](CONTRIBUTING.md) how to send a change.

## License

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later, at your
option. [LICENSE](LICENSE) has the terms. Each release publishes
`THIRD-PARTY.md` beside its image, listing the crates the image links in,
with their licenses.
