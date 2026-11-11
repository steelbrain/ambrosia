# Graphics drivers

Ambrosia has no display driver of its own. A graphics driver module, named
by the configuration, sets up one display device and gives Ambrosia a
framebuffer. Ambrosia draws its log there as an early console, then hands
the framebuffer to the kernel, whose console goes on drawing there. On an
x86-64 PC a device's option ROM does the same through UEFI GOP.

This page is the interface for module authors.
[Writing a graphics driver module](how-to/write-a-graphics-driver.md) walks
through making one. The types are in
[`crates/ambrosia/src/graphics.rs`](../crates/ambrosia/src/graphics.rs),
the loader in [`elf.rs`](../crates/ambrosia/src/elf.rs), and the example
module, for QEMU's `bochs-display`, in
[`drivers/bochs-display`](../drivers/bochs-display/README.md).

## How a module runs

1. `config.plist` names the module as `Graphics` > `Driver`, a path in the
   configuration bundle ([configuration](configuration.md#graphics--driver)).
   A path the bundle doesn't hold stops the boot, as does a file the loader
   refuses.
2. Once it has read the configuration, and before it reads the guest's
   disks, Ambrosia loads the module into its heap and calls its entry
   point, once.
3. If the module returns 0 with a framebuffer Ambrosia can draw on,
   Ambrosia clears it to black and draws every later log line there as
   well as on the UART. It gives the framebuffer to the kernel in the boot
   arguments' video fields. If the configuration names a
   `Graphics` > `DisplayNode`, Ambrosia also adds that device tree node,
   for a kernel display driver
   ([configuration](configuration.md#graphics--displaynode)).
4. Ambrosia never calls the module again, and nothing stops the device. It
   stays as the module left it, for the kernel.

## The file

A module is an ELF64 little-endian aarch64 static PIE, of type `ET_DYN`:

- `PT_LOAD` segments, each aligned to a power of two. Ambrosia copies them
  to a base aligned to the largest alignment, and zeroes the rest of their
  memory, the bss.
- Relocations only of type `R_AARCH64_RELATIVE`, against no symbol, in a
  `DT_RELA` table of 24-byte entries that `PT_DYNAMIC` names and a loaded
  segment holds. Ambrosia writes base plus addend at each.
- `e_entry` in an executable segment.

The loader refuses, with an error that names the problem:

- what needs a dynamic linker: `PT_INTERP`, `DT_NEEDED` and PLT
  relocations (`DT_JMPREL`, a non-zero `DT_PLTRELSZ`);
- thread-local storage, `PT_TLS`;
- `DT_REL` and `DT_RELR` tables, and any other relocation type;
- a relocation that writes outside the image.

It ignores `PT_PHDR`, `PT_GNU_RELRO`, `PT_GNU_STACK` and
`PT_GNU_EH_FRAME`, which ask nothing of a loader that leaves all of RAM
readable, writable and executable.

Rust links such a file for `aarch64-unknown-none` with these flags, as the
example's `.cargo/config.toml` sets them:

```toml
[build]
target = "aarch64-unknown-none"

[target.aarch64-unknown-none]
rustflags = [
    "-C", "relocation-model=pie",
    "-C", "link-arg=-pie",
    "-C", "link-arg=--entry=driver_entry",
]
```

rust-lld's output has 64 KiB-aligned segments and only
`R_AARCH64_RELATIVE` relocations. `graphics.rs` uses nothing but `core`, so
a Rust module can include it as it is, with `#[path]`. The interface is
C's, so a module can be written in any language whose toolchain produces
the file above. Only rust-lld's output has been tried.

## The entry point

Revision 1, in C:

```c
struct services {                  // offset
    uint32_t revision;             //  0: 1
    uint32_t reserved;             //  4: 0
    uint64_t ecam_base;            //  8
    uint64_t ecam_size;            // 16
    uint64_t window_base;          // 24
    uint64_t window_size;          // 32
    void (*log)(const struct services *services,
                const uint8_t *text, size_t len);   // 40
};                                 // 48 bytes

struct framebuffer {               // offset
    uint64_t base;                 //  0
    uint32_t width;                //  8
    uint32_t height;               // 12
    uint32_t stride;               // 16
    uint32_t reserved;             // 20
};                                 // 24 bytes

int32_t entry(const struct services *services, struct framebuffer *out);
```

- `revision` is 1. A later revision only adds fields at the end of
  `services`, so a module checks that it is at least the one it needs.
- `ecam_base` and `ecam_size` give the PCIe ECAM. A function's
  configuration space is 4 KiB at
  `ecam_base + (bus << 20) + (device << 15) + (function << 12)`. On
  `vmapple` the ECAM is at 0x40000000, 256 MiB, and every device is on
  bus 0.
- `window_base` and `window_size` give the PCIe memory window. The module
  gives its device's BARs addresses from it, and the kernel gives every
  other BAR its own. On `vmapple` it runs from 0x50000000 up to 4 GiB.
- `log` writes `len` bytes of UTF-8 as one line of Ambrosia's log. `text`
  may be null when `len` is 0. Ambrosia adds its prefix, `ambrosia: `, and
  the line's end. At 1024 pixels the console is 64 columns wide, so a line
  of up to 54 characters fits on one row.
- The entry point returns 0 when it drives a display and has filled `out`,
  which Ambrosia zeroes before the call. Any other value means it found no
  device it drives, or failed. Ambrosia then logs the value and boots on
  with no console but the UART:

  ```text
  ambrosia: graphics driver: no display (1)
  ```

## Where the module runs

- At EL1, on the boot CPU, with the other CPUs off and interrupts masked.
- With the MMU on and an identity map, so physical addresses are its
  pointers. RAM is normal write-back memory and executable. The rest of
  the first 4 GiB, the ECAM and the PCIe window among it, is device memory
  and not executable. Each access there must be aligned to its size, with
  no `dc zva` and no exclusive or atomic accesses. Rust's
  `aarch64-unknown-none` target builds with strict alignment. Each access
  to a register must also be one instruction of a form the CPU reports
  with a syndrome ([below](#accessing-device-registers)).
- With FP and SIMD on.
- On Ambrosia's stack, with more than 32 KiB of it free at the call.
- With the virtual counter, `CNTVCT_EL0`, and its frequency, `CNTFRQ_EL0`,
  readable, for delays.

An exception in the module stops the boot: Ambrosia's vectors log it with
the registers and halt ([the log](boot-process.md#the-log)).

## Accessing device registers

The VMM emulates the ECAM and the device's registers: each load or store
there traps to it, and it needs the access's size, direction and register.
The CPU gives it those, in the abort's syndrome (ISV=1), only for a load or
store of one general-purpose register with an immediate or register
offset, such as `ldr w0, [x1]` or `strh w0, [x1, #0x502]`. Pairs (`ldp`,
`stp`), SIMD&FP registers, and the pre- and post-indexed forms, which
write the address back, such as `ldr w9, [x8, #0x10]!`, come without one.
The VMM must then read the instruction from guest memory and emulate it,
and one that doesn't stops, or injects an abort, which stops the boot
([the machine](machine.md#mmio-accesses-without-a-syndrome-under-hvf)).

A volatile access, in Rust or C, fixes each access's size and number, but
not its instruction, and whether the compiler folds an address update into
one depends on the code around it. clang compiles a C function that sizes a
BAR through `volatile uint32_t *` to `ldr w9, [x8, #0x10]!` on its own, and
to `ldr w8, [x0, #0x10]` where it is inlined with a constant index.

So make each access in inline assembly, which fixes the instruction. The
example does it in Rust, in
[`src/mmio.rs`](../drivers/bochs-display/src/mmio.rs). In C:

```c
static inline uint32_t read32(uint64_t at) {
    uint32_t v;
    __asm__ volatile("ldr %w0, [%1]" : "=r"(v) : "r"(at) : "memory");
    return v;
}

static inline void write32(uint64_t at, uint32_t v) {
    __asm__ volatile("str %w0, [%1]" : : "r"(v), "r"(at) : "memory");
}
```

`ldrh` and `strh` make 16-bit accesses, `ldrb` and `strb` 8-bit ones, and
`ldr` and `str` with `%x0` 64-bit ones.

## What a module may and must not do

It may:

- read and write the configuration space of the device it drives;
- size that device's BARs, give them addresses in the window, and turn on
  its memory decoding;
- read and write the device's BARs, and read the counter.

It must not:

- touch any other device's configuration space or BARs. The kernel assigns
  those BARs, and must find the rest of the bus as it would with no
  module.
- write RAM outside its own image and its stack frames. Ambrosia's heap,
  the configuration bundle and the boot's layout are all in RAM.
- change system registers: the MMU and caches, the vectors, the interrupt
  masks or the pointer authentication keys.
- leave anything the device needs in its own memory. Ambrosia never calls
  it again, and the kernel takes over the memory its image is in.

It must:

- return;
- leave the framebuffer's BAR decoding, at the address it gave it, and the
  mode set, since the kernel's console draws there;
- give its BARs addresses the kernel keeps ([below](#where-the-bars-go)).

## The framebuffer

- 32 bits a pixel, `0x00RRGGBB` little-endian, so blue is the first byte in
  memory. It is the format the kernel's console takes for depth 32.
- `base` is physical, inside the window, in one of the device's BARs.
  `base` and `stride` are multiples of 4, and `stride` is at least
  `4 * width`. The framebuffer holds at least one character cell, 16 by 16
  pixels, and lies wholly inside the window. Ambrosia checks all of these,
  and if one fails, logs why and boots on without a console.
- Ambrosia's console draws font8x8's glyphs at twice their size, light
  grey on black: 64 columns by 48 rows at 1024 by 768. It never reads the
  framebuffer back. Long lines wrap, and at the bottom it starts again at
  the top, clearing each row it moves to and the one after.
- The kernel gets the framebuffer in the boot arguments' `Video` fields:
  `v_baseAddr` the base, `v_display` 1, `v_rowBytes` the stride, `v_width`
  and `v_height`, and `v_depth` 32, which is scale 1.

## What the kernel draws

The kernel takes a framebuffer outside RAM, in a PCI BAR. It draws its
progress bar there, centred near the bottom, under Ambrosia's log, and
advances it as the boot goes on. It draws no Apple logo and doesn't clear
the screen. With the boot argument `-v` the screen is the same: the
progress bar and no text.

## Where the BARs go

- **The kernel keeps a BAR the module placed** inside the window, and
  places the other devices' BARs around it. With the example, bochs-display's
  16 MiB BAR 0 stays at 0x50000000 and its 4 KiB BAR 2 at 0x51000000, and
  the kernel puts the virtio and xHCI BARs from 0x51001000 up.
- **QEMU never maps a 32-bit BAR that reaches 0xffffffff**, so the
  console's writes there go nowhere, and the kernel moves such a BAR.
- **The kernel's console doesn't follow a moved BAR.** It goes on drawing
  at the address the boot arguments gave, over whatever the kernel placed
  there since.

So a module places its BARs inside the window, each aligned to its size,
apart from each other, and a 32-bit BAR short of 0xffffffff. The example
places them at the window's start. A 1 GiB BAR fits at 0x80000000, where
the kernel would place one itself, and the kernel keeps one a module puts
there.

## The example: bochs-display

[`drivers/bochs-display`](../drivers/bochs-display/README.md) drives QEMU's
`bochs-display`, PCI 1234:1111. Its BAR 0 is the framebuffer, and its BAR 2
holds the Bochs VBE ("DISPI") registers, at 0x500 plus twice the index,
and QEMU's framebuffer byte-order register at 0x604. The module:

1. finds the device on bus 0;
2. turns its memory decoding off and sizes both BARs;
3. places BAR 0 at the window's start and BAR 2 after it, each aligned to
   its size, and turns memory decoding on;
4. checks the DISPI ID and the video memory size;
5. sets 1024 by 768 at 32 bits, little-endian, with the linear framebuffer
   enabled.

It returns 1 when no device is on bus 0, and 2 when it can't drive the
device it found, and logs why.

`make bochs-display` builds it as `target/bochs-display.elf`, and
`make bochs-display-bundle` builds `target/bochs-display.cpio`, a bundle
whose `config.plist` names it. QEMU runs it with these arguments beside the
usual ones ([running](running.md)):

```sh
-device bochs-display,romfile= \
-device loader,file=target/bochs-display.cpio,addr=0x120000000,force-raw=on
```

With `scripts/run-guest.sh`:

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin \
  -b target/bochs-display.cpio path/to/vm -- -device bochs-display,romfile=
```

`romfile=` drops the device's x86 VGA BIOS, of no use to an arm64 guest.
With `-display none`, QMP's `screendump` shows the screen.

To load a module through the loader on the host, set
`AMBROSIA_TEST_DRIVER` to its absolute path and run the loader test;
`make test` does this with the example:

```sh
AMBROSIA_TEST_DRIVER=$PWD/target/bochs-display.elf \
  cargo test -p ambrosia --test driver
```

### A boot with the example

A 26.6.2 guest's log, with the guest's identifiers replaced:

```text
ambrosia: Ambrosia 0.1.1
ambrosia: 4 CPUs, RAM 0x100000000-0x300000000, ECID 0x<ECID>
ambrosia: configuration: 14748 bytes, 0 kexts, 0 patches, graphics driver Drivers/bochs-display.elf
ambrosia: graphics driver Drivers/bochs-display.elf: 0x32280 bytes at 0x140010000
ambrosia: bochs-display at 00:05.0: 1024x768, framebuffer 0x50000000, registers 0x51000000
ambrosia: Ambrosia 0.1.1: early console, 1024x768 at 0x50000000
ambrosia: macOS <volume group UUID>, boot objects <manifest hash>
ambrosia: NVRAM bank 1 of 2, generation 176
ambrosia: kernel collection: 219 entries, 0x4ac8000 bytes at 0x1006f0000
ambrosia: 19 PAC spins made nops
ambrosia: physBase 0x100000000 virtBase 0xfffffe0022000000 top 0x10524c000 memSize 0x1ffb38000 slide 0x1b6ec000
ambrosia: entering the kernel at 0x103304480
```

The screen shows the log from the early console's first line to the
kernel's entry, then the kernel's progress bar beneath it. The guest
answers SSH about as soon as a plain guest does. macOS has no driver for
`bochs-display`, so the kernel's console is all the screen ever shows.
