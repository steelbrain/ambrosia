# ambrosia-bundle

A command-line tool that builds, checks and lists Ambrosia's configuration
bundles. A bundle is a cpio archive in the "newc" format whose first file is
`config.plist`. QEMU's generic loader puts it in the guest's RAM at
0x120000000, where Ambrosia reads it
([configuration](../../docs/configuration.md)).

The tool reads archives with the firmware's own readers, from the
[`ambrosia`](../ambrosia/README.md) crate, so a bundle it accepts is one the
firmware reads and lays out. What depends on the guest, such as the symbols
a kext imports, it can't check ([the checks](#the-checks)). It runs on macOS
and Linux.

## Commands

```sh
ambrosia-bundle create --config config.plist [--kext path/X.kext]... [--driver path/file.elf]... --output bundle.cpio
ambrosia-bundle check bundle.cpio
ambrosia-bundle list bundle.cpio
```

- `create` builds a bundle from files on disk, checks it, and writes it to
  `--output` only if it passes. `--kext` and `--driver` may each be given
  any number of times.
- `check` checks an existing bundle, however it was built.
- `list` prints each file the firmware reads from a bundle, with its size
  in bytes. Directories, which the firmware skips, are not listed.

`create` and `check` print what the firmware will find, in the terms of the
`configuration:` line of its boot log, then one line for each kext it will
place:

```text
bundle.cpio: 15012 bytes, 2 files, 0 kexts, 0 patches, graphics driver Drivers/bochs-display.elf
```

The exit status is 0 on success, 1 when a bundle is refused or a file can't
be read or written, and 2 when the command line is wrong.

## The archive

`create` lays the inputs out where a configuration names them:

- the `--config` file as `config.plist`, whatever its own name, first;
- each `--driver` file at `Drivers/<file name>`;
- each `--kext` directory at `Kexts/<name>.kext`, with every file and
  directory in it;
- the trailer.

After `config.plist`, the members are sorted by path, one path component at
a time, so each directory comes just before what it holds. A directory has
mode 0755. A file has mode 0755 if its user-execute bit, 0100, is set, and
0644 otherwise. Every member has mtime 0, uid and gid 0, and device numbers
0. The same inputs therefore give the same bytes on any machine.

Each `--kext` must be a directory whose name ends in `.kext`, holding
`Contents/Info.plist`. It may hold only files and directories: `create`
refuses a symbolic link or other special file in it, since the firmware
reads only regular files. Two inputs that would go in at the same path, such
as two kexts of the same name, are refused too.

## The checks

`create` checks the archive it built, and `check` the one it is given:

- The archive is at most 512 MiB, the space from 0x120000000 to the
  firmware's heap, and `ambrosia::bundle` reads it, with `config.plist`
  first.
- `config.plist` is an XML property list that `ambrosia::config` reads:
  every key it knows has the right type, and each patch's `Find` is not
  empty and is as long as its `Replace`.
- Each enabled entry of `Kernel` > `Add` has `Contents/Info.plist` under
  its `BundlePath`, with a `CFBundleIdentifier`, and the executable its
  `CFBundleExecutable` names under `Contents/MacOS/`.
- That executable is a thin arm64e Mach-O, and `ambrosia::kext` reads it:
  an `MH_KEXT_BUNDLE` with chained fixups and a `_kmod_info` symbol, whose
  header and load commands lie in its segment at file offset 0.
- `ambrosia::auxkc` can lay the kexts out in the Auxiliary kernel
  collection, as the firmware sizes it before it reads the guest's disk:
  each executable's segments with contents, but `__LINKEDIT`, start with the
  one at file offset 0 and are each `__TEXT`, `__TEXT_EXEC`,
  `__DATA_CONST`, `__OBJC_CONST` or `__DATA`, and the fields of its
  `kmod_info` that the firmware writes lie in one of them.
- The file `Graphics` > `Driver` names is in the archive, and passes the
  checks `ambrosia::elf` makes before it loads a module.
- Every kext directly under `Kexts/` is named by an entry of `Kernel` >
  `Add`, enabled or not: by its own path, or by the path of a kext inside
  it.

The firmware makes the same checks as it boots, and stops the boot on a
failure, with two exceptions: it doesn't check a kext's CPU type, and it
ignores a kext that `config.plist` doesn't name. The tool refuses both: a
kext built for another CPU can't run in an arm64e kernel, and a kext that
`config.plist` doesn't name is never placed.

The tool can't check what depends on the guest, which it doesn't have, and
it sizes the Auxiliary kernel collection without building it. The firmware
checks these only at boot:

- each symbol a kext imports, which the kernel collection or another kext
  in the configuration must define;
- each patch's `Identifier`, which must name an entry of the kernel
  collection;
- each symbol `ExportVtableDiscriminators` names, which the kernel
  collection must define;
- `Graphics` > `DisplayNode`, which must not name a node the guest's
  device tree already has under `/arm-io`;
- that the Auxiliary kernel collection and the guest's trust caches fit in
  RAM below the kernel collection;
- that the collection's load commands, with a fileset entry for each kext,
  fit in its 16 KiB header, and the kexts' chained fixups in its 16 KiB
  `__LINKEDIT`.

Each refusal says what is wrong and what to change:

```text
ambrosia-bundle: Kexts/Example.kext is in the bundle, but config.plist doesn't name it; add an entry for it to Kernel.Add: <dict><key>BundlePath</key><string>Kexts/Example.kext</string></dict>
```

## Building and testing

From the repository's root:

```sh
cargo build --release -p ambrosia-bundle      # target/release/ambrosia-bundle
cargo install --path crates/ambrosia-bundle   # or install it with Cargo
make test                                     # its tests, with the others
```

`make bochs-display-bundle` and `make hello-kext-bundle` build the example
bundles with it.

## Source

- `src/main.rs`: the command line.
- `src/create.rs`: gathers the inputs from disk and lays them out.
- `src/newc.rs`: writes the archive.
- `src/check.rs`: the checks.
- `src/error.rs`: each refusal and its message.
- `tests/bundle.rs`: builds bundles on disk, reads them back as the
  firmware does, and checks each refusal. With `AMBROSIA_TEST_DRIVER` set to
  a driver module's ELF file, as `make test` sets it, it also builds the
  example bundle.

## Documentation

- [Build a bundle](../../docs/how-to/build-a-bundle.md): writing a
  configuration, building the bundle, and booting with it.
- [Configuration](../../docs/configuration.md): the bundle and every key of
  `config.plist`.

Ambrosia is licensed under GPL-2.0-or-later OR LGPL-3.0-or-later; see
[LICENSE](../../LICENSE).
