# The handover to injected code

When Ambrosia places kexts, it tells the code in them where everything
went. It records the kernel collection's base and each placed kext's
address and size in the device tree, under `/chosen/ambrosia`. `/chosen`
survives into the I/O Registry's device tree plane, so a kext, or a
framework that patches the kernel from inside it, reads the record at run
time with the I/O Registry. The addresses are slid virtual addresses, the
same ones the kernel's OSKext reports.

The node is written only when Ambrosia placed a kext or exported vtable
discriminators, so a plain guest's device tree has none. It names nothing
of any particular product: any framework can read it.

## The node

`/chosen/ambrosia` has one property:

- `boot-kc-base`, 64 bits: the slid virtual address of the Boot KC's
  `mach_header`, for code that binds against the Boot KC's symbols.

It has one child for each placed kext, named by the kext's
`CFBundleIdentifier`, with:

- `bundle-id`, a string: the `CFBundleIdentifier`;
- `base`, 64 bits: the slid virtual address of the kext's `mach_header`,
  inside the Auxiliary KC;
- `size`, 64 bits: the span from the kext's lowest-addressed segment to the
  end of its highest.

When the configuration lists vtable symbols, it also has a `vtable-disc`
child ([below](#vtable-discriminators)).

Every integer is little-endian, as the device tree stores it.

## Reading it

In the kernel, look the node up in the device tree plane and read its
properties as `OSData`:

```cpp
IORegistryEntry *node =
    IORegistryEntry::fromPath("/chosen/ambrosia", gIODTPlane);
```

From user space in the guest, `ioreg` shows it:

```sh
ioreg -p IODeviceTree -r -n ambrosia -l
```

## Finding a placed kext's segments

`base` is the address of the kext's own Mach-O header, and that header is
the map of where the kext's bytes went. They are not where the kext's own
file put them. Ambrosia builds the Auxiliary KC by merging the kexts'
segments by name, in the order `__TEXT`, `__TEXT_EXEC`, `__DATA_CONST`,
`__DATA`: every kext's `__TEXT` lies together, then every kext's code, and
so on. A kext's segments are therefore not contiguous, and not at `base`
plus the offsets in its own file.

Ambrosia rewrites each placed kext's header so that its `LC_SEGMENT_64`
commands give the addresses its segments took, as offsets from the
collection's start, and marks the header `MH_DYLIB_IN_CACHE`, which tells
the kernel to slide those addresses in place as it loads the collection.
So read the header at `base`, follow its segment commands, and compare its
`__TEXT` segment's address with `base`:

- if they are equal, the addresses are already slid, and each is where
  that segment is at run time;
- otherwise, `base` less the `__TEXT` address is the collection's load
  address, and each segment is at that load address plus its address in
  the header.

`size` bounds a kext's footprint, but since the kexts' segments interleave,
the spans of two kexts overlap, and a span holds other kexts' bytes too.
The segment commands are the exact map.

## Vtable discriminators

On arm64e a C++ vtable's method pointers are signed with the IA key, with
address diversity, and with a 16-bit discriminator taken from the method's
type. Code that defines a subclass of a kernel class at run time, in
memory Ambrosia didn't lay out, must sign each slot of the new vtable
itself, for the slot's own address. The running kernel holds only the
signed pointers, from which the discriminator can't be read back. It is
in the Boot KC's chained fixup for each slot, which the kernel consumes as
it applies its fixups.

So when the configuration lists vtable symbols under `Kernel` >
`ExportVtableDiscriminators`, Ambrosia reads those fixups before the kernel
runs ([`vtdisc.rs`](../../crates/ambrosia/src/vtdisc.rs)), and
`/chosen/ambrosia` gets a `vtable-disc` child with one child per symbol:

- `symbol`, a string: the vtable's symbol, such as
  `__ZTV25IOMobileFramebufferLegacy`;
- `vtable-base`, 64 bits: the slid virtual address of the vtable's first
  word, which a reader can check against the vtable it resolves itself;
- `first-slot`, 32 bits: 2, the first method slot after the vtable's
  offset-to-top and typeinfo words;
- `slots`, data: one 8-byte little-endian record per method slot, in
  order from `first-slot`:

  ```c
  struct slot {
      uint16_t slot;      // the vtable word index
      uint16_t div;       // the discriminator
      uint8_t  key;       // 0 IA, 1 IB, 2 DA, 3 DB
      uint8_t  addr_div;  // 1 if the slot's address is blended in
      uint8_t  pad[2];
  };
  ```

**Take each slot from the most-base class.** A slot's discriminator is not
always the one in the subclass's nearest base. When a class declares an
inherited method pure again, its own vtable signs that slot with a
discriminator of its own, while callers, and every concrete override, use
the first declaration's. A slot re-signed with the redeclaring class's
value fails to authenticate when it is called, and the kernel takes an
instruction fetch abort. So list the base classes' vtables too, and take
each slot from the most-base listed vtable that has it.

## What the handover doesn't do

Ambrosia places kexts and records where they went. It does no binding,
routing or patching inside the running kernel: that is the injected code's
job. The pointer authentication keys the injected code runs on are the
ones the kernel runs on, one set shared by the kernel and every process
([the keys](replacing-iboot.md#the-keys)).
