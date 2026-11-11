# bochs-display

An Ambrosia graphics driver module for QEMU's `bochs-display`, PCI
1234:1111, and the example of the [driver module
interface](../../docs/graphics-drivers.md). It finds the device on bus 0,
places its two BARs at the start of the PCIe window, sets 1024 by 768 at
32 bits, and returns the framebuffer. Ambrosia's log then shows on the
device's screen, followed by the kernel's progress bar.

The device is QEMU's own, so the module works on any QEMU machine that has
it on bus 0. macOS has no driver for it: the screen shows the boot and
nothing after.

## Files

- `src/main.rs`: the module, with its entry point, `driver_entry`. It
  includes the interface's types from
  [`crates/ambrosia/src/graphics.rs`](../../crates/ambrosia/src/graphics.rs)
  as they are.
- `src/mmio.rs`: the loads and stores of the device's registers, in inline
  assembly, so that each is one instruction of a form the CPU reports with
  a syndrome
  ([accessing device registers](../../docs/graphics-drivers.md#accessing-device-registers)).
- `.cargo/config.toml`: builds for `aarch64-unknown-none` as a static PIE,
  with `driver_entry` as the ELF's entry point.
- `config.plist`: a configuration that starts the module from
  `Drivers/bochs-display.elf` in the bundle.
- `Cargo.toml`: the crate is a workspace of its own, apart from the
  image's, since a module is a program of its own.

The entry point returns 0 with the framebuffer filled in, 1 when no
`bochs-display` is on bus 0, and 2 when it can't drive the one it found.
It logs a line through Ambrosia either way.

## Building

From the repository's root:

```sh
make bochs-display          # target/bochs-display.elf
make bochs-display-bundle   # target/bochs-display.cpio, with config.plist
make test                   # loads the module through Ambrosia's loader
make clippy                 # lints it with the other crates
```

Make builds it from this directory, so that `.cargo/config.toml` applies.

## Running

Give QEMU the device and the bundle, beside the usual arguments
([running](../../docs/running.md)):

```sh
-device bochs-display,romfile= \
-device loader,file=target/bochs-display.cpio,addr=0x120000000,force-raw=on
```

`romfile=` drops the device's x86 VGA BIOS, of no use to an arm64 guest.
With `scripts/run-guest.sh`:

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin \
  -b target/bochs-display.cpio path/to/vm -- -device bochs-display,romfile=
```

With `-display none`, QMP's `screendump` shows the screen.

## Documentation

- [Graphics drivers](../../docs/graphics-drivers.md): the interface this
  module implements, and what a boot with it shows.
- [Writing a graphics driver module](../../docs/how-to/write-a-graphics-driver.md):
  how to make a module of your own from this one.
- [Configuration](../../docs/configuration.md#graphics--driver): the
  `Graphics` > `Driver` key.

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later; see
[LICENSE](../../LICENSE).
