# Writing a graphics driver module

A graphics driver module sets up one display device before macOS starts,
so that Ambrosia's log shows on it and the kernel's console goes on
drawing there. This page walks through making one, starting from the
example for QEMU's `bochs-display`. [Graphics drivers](../graphics-drivers.md)
is the full interface: the file format, the entry point, the rules a module
follows and what the kernel does with its framebuffer.

## Start from the example

[`drivers/bochs-display`](../../drivers/bochs-display/README.md) is a
complete module in about 270 lines of Rust. Copy its directory to start a
new one:

```sh
cp -R drivers/bochs-display drivers/my-display
```

Then, in the copy:

1. Rename the crate and its binary in `Cargo.toml`. The crate is a
   workspace of its own, since a module is a program of its own, not part
   of the firmware.
2. Keep `.cargo/config.toml`. It builds for `aarch64-unknown-none` as a
   static PIE, with `driver_entry` as the ELF's entry point.
3. Keep the `#[path]` line in `src/main.rs` that includes
   `crates/ambrosia/src/graphics.rs`, the interface's types, as they are.
   Fix the relative path if the directory is elsewhere.
4. Change `VENDOR` and `DEVICE` to your device's PCI IDs, and replace the
   mode setting with your device's.

## Write the entry point

The entry point gets the `Services` Ambrosia passes and a `Framebuffer` to
fill. In order, it:

1. checks that `services.revision` is at least the revision it was written
   for, 1;
2. finds its device in the ECAM, on bus 0, by its vendor and device IDs;
3. turns the device's memory decoding off, sizes the BARs it needs, gives
   them addresses inside the window, each aligned to its size, and turns
   decoding back on;
4. sets a mode of 32 bits a pixel, `0x00RRGGBB` little-endian;
5. fills the framebuffer's base, width, height and stride, and returns 0.

It returns any other value when it finds no device or can't drive the one
it found, and Ambrosia boots on without a console. Through
`services.log`, it can add lines to Ambrosia's log, which help when a
module returns early.

Keep to the rules in
[what a module may and must not do](../graphics-drivers.md#what-a-module-may-and-must-not-do).
The ones that bite most often:

- Access device memory only with aligned, single loads and stores, such as
  Rust's `read_volatile` and `write_volatile`. Device memory faults on an
  unaligned or exclusive access.
- Give BARs addresses the kernel keeps: inside the window, apart from each
  other, and a 32-bit BAR short of 0xffffffff
  ([where the BARs go](../graphics-drivers.md#where-the-bars-go)).
- Leave the device running, with the mode set and the framebuffer's BAR
  decoding, since the kernel's console draws there after Ambrosia.

## Build it and load it on the host

From the module's directory:

```sh
cargo build --release
```

The ELF is at `target/aarch64-unknown-none/release/<name>`. Before booting
a guest, load it through Ambrosia's own loader on the host. The loader test
checks the file, copies its segments and applies its relocations, as the
firmware does:

```sh
AMBROSIA_TEST_DRIVER=/absolute/path/to/my-display.elf \
  cargo test -p ambrosia --test driver
```

The path must be absolute. A file the loader refuses fails the test with
the loader's error, the one the boot would stop with.

## Put it in a bundle

Write a `config.plist` that names the module by its path in the bundle,
under `Drivers/`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Graphics</key>
  <dict>
    <key>Driver</key>
    <string>Drivers/my-display.elf</string>
  </dict>
</dict>
</plist>
```

Then build the bundle with `ambrosia-bundle`, which puts the module at
`Drivers/<file>` ([building a bundle](build-a-bundle.md)):

```sh
cp target/aarch64-unknown-none/release/my-display my-display.elf
ambrosia-bundle create --config config.plist --driver my-display.elf \
    --output my-display.cpio
```

## Boot with it

Give QEMU the bundle and the device:

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin \
  -b my-display.cpio path/to/vm -- -device my-display-device
```

Ambrosia logs where it loaded the module, then the module's own lines, then
the early console's first line:

```text
ambrosia: graphics driver Drivers/my-display.elf: <size> bytes at <base>
ambrosia: Ambrosia 0.1.0: early console, 1024x768 at 0x50000000
```

With `-display none`, QMP's `screendump` shows the screen.

## When it goes wrong

- `graphics driver: no display (n)`: the entry point returned `n`, not 0.
- `graphics driver: its framebuffer, <width>x<height> at <base> with stride <stride>, can't be used: ...`:
  the entry point returned 0, but the framebuffer it gave failed Ambrosia's
  checks, such as lying outside the window
  ([the framebuffer](../graphics-drivers.md#the-framebuffer)). The line
  ends with the check it failed, such as
  `it is not wholly inside the PCIe window`.
- `bad graphics driver <path>: ...`: the loader refused the file. The
  error names the problem, such as a relocation type it doesn't apply.
- An `exception` line: the module faulted. `ELR` less the base in the
  `graphics driver ... bytes at <base>` line is the address in the module's
  own ELF ([reading an exception](../../crates/firmware/README.md#reading-an-exception)).
- The log has the early console's line, but the screen stays black: QEMU
  never maps a 32-bit BAR that reaches 0xffffffff, so the console's writes
  go nowhere, and the kernel later moves the BAR while its console draws at
  the old address
  ([where the BARs go](../graphics-drivers.md#where-the-bars-go)).
