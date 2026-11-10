# Build a configuration bundle

This guide builds a configuration bundle that adds a kext and starts a
graphics driver module, checks it, and boots a guest with it. A bundle is a
cpio archive with `config.plist` first, which QEMU loads into the guest's
RAM for Ambrosia to read. [Configuration](../configuration.md) describes
every key `config.plist` may hold.

The kext is the example in
[`examples/hello-kext`](../../examples/hello-kext), and the driver module
the example for QEMU's `bochs-display`, in
[`drivers/bochs-display`](../../drivers/bochs-display). You need:

- Ambrosia's image, `target/ambrosia.bin`, which `make` builds
  ([building](../building.md));
- a guest that boots on Ambrosia, and its ECID
  ([running a guest](../running.md));
- the macOS SDK, which Xcode or its Command Line Tools install, to build
  the example kext;
- `ambrosia-bundle`, which this command, run in the repository, builds as
  `target/release/ambrosia-bundle` ([building](../building.md#the-bundle-tool)):

```sh
cargo build --release -p ambrosia-bundle
```

The commands below run it as `ambrosia-bundle`: put it on your `PATH`, or
give its path.

## 1. Write config.plist

`ambrosia-bundle` puts each kext at `Kexts/<name>.kext` and each driver
module at `Drivers/<file name>`, so `config.plist` names them by those
paths. This one adds `Hello.kext` and starts the bochs-display module:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Kernel</key>
	<dict>
		<key>Add</key>
		<array>
			<dict>
				<key>BundlePath</key>
				<string>Kexts/Hello.kext</string>
			</dict>
		</array>
	</dict>
	<key>Graphics</key>
	<dict>
		<key>Driver</key>
		<string>Drivers/bochs-display.elf</string>
	</dict>
</dict>
</plist>
```

With several kexts, list them in `Kernel` > `Add` in the order the kernel
should consider them: a base kext before the plugins that import its
symbols ([Kernel > Add](../configuration.md#kernel--add)).

## 2. Gather the kexts and driver modules

A kext is its bundle directory: `Contents/Info.plist`, and its executable
under `Contents/MacOS/`, a thin arm64e kext bundle with chained fixups.
[Adding a kext](add-a-kext.md#building-a-kext) says what Ambrosia needs of
one. A driver module is one ELF file in the format
[graphics drivers](../graphics-drivers.md#the-file) describes.

Build the two examples:

```sh
make hello-kext      # target/hello-kext/Hello.kext
make bochs-display   # target/bochs-display.elf
```

## 3. Create the bundle

```sh
ambrosia-bundle create --config config.plist \
  --kext target/hello-kext/Hello.kext \
  --driver target/bochs-display.elf \
  --output bundle.cpio
```

`create` checks the bundle with the firmware's own readers before it writes
it, and prints what Ambrosia will find there:

```text
bundle.cpio: 116344 bytes, 4 files, 1 kext, 0 patches, graphics driver Drivers/bochs-display.elf
  kext ai.aneesiqbal.ambrosia.example.hello at Kexts/Hello.kext
```

The size depends on the compiler that built the kext. The same inputs give
the same bytes, on macOS and on Linux.

If anything would stop the boot, `create` writes nothing and says what to
change. Given a second kext that `config.plist` doesn't name, it prints:

```text
ambrosia-bundle: Kexts/Other.kext is in the bundle, but config.plist doesn't name it; add an entry for it to Kernel.Add: <dict><key>BundlePath</key><string>Kexts/Other.kext</string></dict>
```

## 4. Check the bundle

`check` runs the same checks on a bundle that exists, however it was built.
Both commands read the bundle as Ambrosia does: the archive,
`config.plist` and each kext's Info.plist, each kext's executable and its
layout in the Auxiliary kernel collection, as Ambrosia sizes that
collection, and the driver module, as the loader checks it before it loads
it.

They can't check what depends on the guest, which only the boot has: the
symbols a kext imports, the entries a patch's `Identifier` names, the
symbols `ExportVtableDiscriminators` names, a `DisplayNode` the guest's
device tree already has, and whether the kexts fit in RAM beside the
guest's kernel collection. Nor do they build the Auxiliary kernel
collection, so they don't find kexts whose fileset entries overflow its
16 KiB header, or whose chained fixups overflow its 16 KiB `__LINKEDIT`. A
problem with any of these stops the boot with a `boot failed:` line
([when the boot stops](add-a-kext.md#when-the-boot-stops)).
[The tool's checks](../../crates/ambrosia-bundle/README.md#the-checks)
lists each.

`list` prints the files Ambrosia reads from a bundle, with their sizes in
bytes:

```sh
ambrosia-bundle check bundle.cpio
ambrosia-bundle list bundle.cpio
```

```text
  450  config.plist
14216  Drivers/bochs-display.elf
 1256  Kexts/Hello.kext/Contents/Info.plist
99096  Kexts/Hello.kext/Contents/MacOS/Hello
4 files, 116344 bytes
```

## 5. Boot with it

`scripts/run-guest.sh -b` loads the bundle at 0x120000000. Arguments after
`--` go to QEMU: here, the display device the module drives, without its
x86 option ROM.

```sh
scripts/run-guest.sh -e "$ECID" -b bundle.cpio path/to/vm -- \
  -device bochs-display,romfile=
```

Without the script, give QEMU the bundle with
`-device loader,file=bundle.cpio,addr=0x120000000,force-raw=on`
([running a guest](../running.md#the-configuration-bundle)).

The serial log shows Ambrosia reading the bundle, starting the module and
placing the kext, before it enters the kernel. The addresses change from
boot to boot:

```text
ambrosia: Ambrosia 0.1.0
ambrosia: configuration: 116344 bytes, 1 kexts, 0 patches, graphics driver Drivers/bochs-display.elf
ambrosia: graphics driver Drivers/bochs-display.elf: 0x322f0 bytes at 0x140010000
ambrosia: Ambrosia 0.1.0: early console, 1024x768 at 0x50000000
...
ambrosia: AuxKC: 131072 bytes, 1 kext(s), 318 fixups, BootKCID <data>
ambrosia: AuxKC: kext ai.aneesiqbal.ambrosia.example.hello at 0x4000 (slid 0xfffffe001537c000), code 0x10000, kmod 0x10000
ambrosia: AuxKC placed at phys 0x101378000, virt 0xfffffe0015378000, 0x20000 bytes, 1 kext(s)
...
ambrosia: entering the kernel at 0x1042bc480
```

Once the guest answers SSH, `kmutil showloaded` lists the kext, and the
I/O Registry has the property the example's service sets when it starts
([checking the guest](add-a-kext.md#checking-the-guest)):

```console
$ kmutil showloaded | grep ai.aneesiqbal.ambrosia.example.hello
  115    0 0                  0x10000    0x10000    ai.aneesiqbal.ambrosia.example.hello (1.0.0) 03FA7678-37EB-3D4E-B5AE-3E4FA181C103 <4 3>
$ ioreg -l | grep Greeting
    | |     "Greeting" = "Hello, world"
```
