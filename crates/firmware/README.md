# ambrosia-firmware

The Ambrosia image: the machine side of the boot. The crate builds a
bare-metal binary for `aarch64-unknown-none`, linked to run from the
`vmapple` machine's firmware region at 0x00100000, and `make image` turns it
into `target/ambrosia.bin`.

It starts the boot CPU, maps memory, finds the configuration bundle,
starts the graphics driver module, and reads the guest's disks through the
machine's firmware disk interface. The core library,
[`ambrosia`](../ambrosia/README.md), lays out the kernel from what it read.
The firmware then sets the pointer authentication keys, turns its MMU off
and enters the kernel. Everything it knows about the machine, its addresses
and its config block, is here and not in the core.

## Modules

- `main.rs`: the boot's sequence, the machine's fixed layout (RAM, the
  PCIe ECAM and window, the bundle's address), the config block, the
  avp-rtc probe, and the reports of an exception or a panic.
- `start.s`: the entry point, the exception vectors, and `enter_kernel`.
- `mmu.rs`: the identity map, and the cache maintenance before the kernel
  runs.
- `heap.rs`: the bump allocator over RAM's second GiB.
- `bdif.rs`: the machine's firmware disk interface, as a `BlockDevice`.
- `mmio.rs`: the loads and stores of device registers, in inline assembly,
  so that the CPU reports each with a syndrome.
- `driver.rs`: loads and calls the graphics driver module.
- `screen.rs`: the early console on the module's framebuffer.
- `uart.rs`: the PL011 log, and the `log!` macro.
- `keys.rs`: writes the pointer authentication keys.
- `link.ld`: the linker script. The image, its data, its bss and its
  64 KiB stack all go in the 1 MiB firmware region.
- `.cargo/config.toml`: builds for `aarch64-unknown-none` and links with
  `link.ld`. It applies when Cargo runs from this directory, as
  `make image` does.

## Building

From the repository's root:

```sh
make          # target/ambrosia.bin
make clippy   # lints, warnings fail
```

The crate has no host tests: it runs only on the machine. The core's tests
cover what it calls ([`ambrosia`](../ambrosia/README.md)).
[Running](../../docs/running.md) shows how to boot a guest with the image.

## Reading an exception

An exception in the firmware or in a driver module stops the boot with a
line like this, then the registers:

```text
ambrosia: exception <index>: ESR <esr> ELR <elr> FAR <far>
```

`ELR` is the address of the instruction that faulted. An address in the
firmware region, from 0x00100000, is in the image. The release ELF keeps
its debug information, so `llvm-objdump`, from the toolchain's
`llvm-tools`, shows the code and source lines there:

```sh
host=$(rustc -vV | sed -n 's/^host: //p')
objdump=$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-objdump
$objdump -dl --start-address=0x109700 --stop-address=0x109740 \
  target/aarch64-unknown-none/release/ambrosia
```

An address in RAM, from 0x100000000, is in the driver module. The log's
`graphics driver <path>: <size> bytes at <base>` line gives where it was
loaded; `ELR` less that base is the address in the module's own ELF.
[Reading the boot log](../../docs/how-to/troubleshoot-a-boot.md) covers
the other lines that stop a boot.

## Documentation

- [The boot process](../../docs/boot-process.md): each step `main` takes.
- [The machine](../../docs/machine.md): the devices and addresses the image
  uses.
- [Building](../../docs/building.md): the toolchain and the Makefile.

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later; see
[LICENSE](../../LICENSE).
