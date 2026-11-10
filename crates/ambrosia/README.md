# ambrosia

The core of Ambrosia: everything the boot does that doesn't touch the
machine. It reads the guest's disks through the `BlockDevice` trait,
unpacks the boot objects iBoot would load, builds an Auxiliary kernel
collection for the configuration's kexts, and lays out the kernel
collection, its device tree and its boot arguments in memory its caller
gives it. The firmware crate, [`ambrosia-firmware`](../firmware/README.md),
gives it the machine.

The crate is `no_std` with `alloc`, so it runs in the firmware. It builds
for the host too, where its tests run, some of them on a real guest's
disk.

## Modules

- `boot`: lays out the kernel collection, trust caches, Auxiliary KC,
  device tree, manifests and boot arguments in RAM, applies the PAC pass
  and the configuration's patches, and gives the kernel's entry. Also
  derives the boot's seed and the PAC keys.
- `auxkc`: builds the Auxiliary kernel collection that holds the
  configuration's kexts.
- `place`: places one kext in that collection: copies its segments,
  re-encodes its `adrp`s and rewrites its header.
- `kext`: reads a kext's executable: its segments, `kmod_info`, imports,
  exports and chained fixups.
- `vtdisc`: reads vtables' pointer-authentication discriminators from the
  kernel collection, for the handover.
- `files`: finds and reads the boot objects on the root disk, and NVRAM on
  the aux disk.
- `block`: `BlockDevice`, a disk read in 512-byte sectors.
- `gpt`: the GUID partition table.
- `apfs`: a read-only APFS reader.
  - `container`: a container's superblock, object map and volumes.
  - `volume`: a volume's file-system tree: paths, directories and files.
  - `btree`: B-trees, and the object map that finds a virtual object.
  - `object`: object headers and their Fletcher-64 checksums.
  - `synthetic`: a small container built in memory, for tests.
- `img4`: Image4 containers: IM4P payloads, unpacked from LZFSE or LZSS,
  and the manifest an IMG4 carries.
- `der`: the DER that Image4 is encoded in.
- `im4m`: Image4 manifests, whose properties go into the device tree.
- `macho`: the kernel collection's `MH_FILESET` header, and its members'
  symbols.
- `adt`: Apple's flattened device tree, parsed, edited and written out.
- `tree`: fills the device tree template as iBoot does, with the
  `/chosen/ambrosia` handover.
- `bootargs`: XNU's `boot_args`.
- `nvram`: NVRAM's banks: finding the current one, and adding a variable.
- `pac`: turns the kernel's PAC spins into `nop`s.
- `bundle`: the configuration bundle, a cpio "newc" archive.
- `config`: `config.plist`, and applying a patch.
- `plist`: XML property lists.
- `sha384`: SHA-384, which names boot objects and derives the boot's seed
  and keys.
- `elf`: loads graphics driver modules, ELF static PIEs.
- `graphics`: the graphics driver interface's types.
- `console`: draws the early console's text on a framebuffer.

It depends on two crates, both `no_std`: `compcol` for LZFSE and `font8x8`
for the console's glyphs. The tests also use `sha2`, for SHA-256.

## Building and testing

From the repository's root:

```sh
make test     # cargo test -p ambrosia, with the example module built
make clippy   # lints, warnings fail
cargo test    # the same tests, without the example module's
```

The crate is a default member of the workspace, so a bare `cargo build`
or `cargo test` at the root builds and tests it on the host.

Some tests read a real guest's files, and are skipped unless these are
set:

- `AMBROSIA_TEST_DISK`: a raw guest disk image. `tests/disk.rs` reads its
  partitions, containers, volumes and boot files, and checks them against
  `tests/fixtures/<image name without extension>.txt`, a record of what
  macOS reads from the same disk. No fixture is committed, since each records
  one guest; `tests/disk.rs` describes the format and how to record one.
- `AMBROSIA_TEST_AUX`: that guest's trimmed aux image. With both set, the
  test reads every boot object and NVRAM, as the firmware does.
- `AMBROSIA_TEST_BOOT_FILES`: a directory of a guest's boot objects, copied
  from its Preboot and iSCPreboot volumes. `tests/boot.rs` lays them out
  and checks the device tree and boot arguments it writes. It holds
  `volume-group` and `boot-dir`, the names as text; `apticket.der`,
  `kernelcache`, `devicetree.img4`, `root_hash.img4`,
  `base_system_root_hash.img4`, `StaticTrustCache.img4` and
  `BaseSystemTrustCache.img4`; `localpolicy.img4` and `sfr-apticket.der`;
  and `nvram.bin`, 1 MiB of the aux disk from its NVRAM offset.
- `AMBROSIA_TEST_DRIVER`: a driver module's ELF file, which
  `tests/driver.rs` loads. `make test` sets it to the example's. The path
  must be absolute, since tests run from this directory.

## Documentation

- [The boot process](../../docs/boot-process.md): what the crate does, step
  by step.
- [How Ambrosia replaces iBoot](../../docs/internals/replacing-iboot.md):
  why it does it that way, the Auxiliary KC in detail.
- [The handover](../../docs/internals/handover.md): what `/chosen/ambrosia`
  records.
- [Configuration](../../docs/configuration.md): the bundle and
  `config.plist`.
- [Graphics drivers](../../docs/graphics-drivers.md): the driver module
  interface.

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later; see
[LICENSE](../../LICENSE).
