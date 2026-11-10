# Adding a kext

Ambrosia adds kexts to a macOS guest's kernel at boot. The configuration
names them, Ambrosia places them in an Auxiliary kernel collection, and the
kernel starts them as it starts any third-party kext. System Integrity
Protection stays enabled, and nothing is done in the guest: no kext
approval, no Reduced Security, no boot arguments.

This page says what a kext needs for Ambrosia to place it, then builds,
bundles and boots the example in
[`examples/hello-kext`](../../examples/hello-kext), and checks from the
guest that the kernel started it.

## What happens to a kext

1. QEMU loads the configuration bundle into the guest's RAM. Ambrosia reads
   `config.plist` from it and, for each entry of
   [`Kernel` > `Add`](../configuration.md#kernel--add), the kext's
   `Contents/Info.plist` and its executable, `Contents/MacOS/` followed by
   the name the Info.plist's `CFBundleExecutable` gives.
2. Ambrosia builds one Auxiliary kernel collection that holds every kext,
   and places it in memory below the kernel collection it loaded from the
   guest's disk. It binds each symbol a kext imports to the symbol of that
   name in the kernel, in one of the kernel collection's kexts, or in
   another kext of the same configuration. It gives the kernel each kext's
   Info.plist keys as they are.
3. Once it is running, the kernel loads the Auxiliary kernel collection,
   rebases and signs the kexts' pointers itself, and treats each kext as
   any kext in an Auxiliary collection: it calls the kext's `kmod_info`
   start routine when it loads it, and starts the IOService classes its
   `IOKitPersonalities` match.

The kext needs no code signature, trust cache entry or `kmutil` step. The
guest runs with developer mode force-enabled, as the `vmapple` machine
reports it, so the kernel accepts the collection with SIP enabled.
[The boot process](../boot-process.md#placing-the-kexts) describes the
collection Ambrosia builds.

## Building a kext

Ambrosia reads a kext executable with these properties:

- **An arm64e kext bundle.** A 64-bit Mach-O of type `MH_KEXT_BUNDLE`,
  which the linker writes when given `-kext`, built for arm64e, the
  architecture of kernel code on Apple silicon.
- **Chained fixups.** The linker's `-fixup_chains` stores the kext's
  pointers as chained fixups in an `LC_DYLD_CHAINED_FIXUPS` load command.
  For an arm64e kext they are in the `DYLD_CHAINED_PTR_ARM64E_KERNEL`
  format, the one Ambrosia reads.
- **Imports by name.** Every symbol the kext uses from the kernel is a
  flat-namespace import, which the linker leaves when given
  `-undefined dynamic_lookup`. Ambrosia binds the imports at boot against
  the kernel collection on the guest's disk, so the build needs only the
  kernel's headers, which the macOS SDK has, and no Kernel Debug Kit.
- **A symbol table with `_kmod_info`.** Ambrosia finds the kext's
  `kmod_info` by that symbol and tells the kernel where it is. Don't strip
  the executable.
- **The segments the linker writes for a kext.** `__TEXT` comes first, at
  file offset 0, and the others are `__TEXT_EXEC`, `__DATA_CONST`,
  `__DATA` and `__LINKEDIT`.

[`build.sh`](../../examples/hello-kext/build.sh) builds the example this
way, with the SDK's `Kernel.framework` headers:

```sh
sdk=$(xcrun --show-sdk-path)
mkdir -p Hello.kext/Contents/MacOS
cp Info.plist Hello.kext/Contents/
xcrun clang -target arm64e-apple-macos11 -x c++ -std=c++17 -Wall \
  -mkernel -fapple-kext -nostdinc \
  -fno-builtin -fno-common -fno-exceptions -fno-rtti -fno-stack-protector \
  -I"$sdk/System/Library/Frameworks/Kernel.framework/Headers" \
  -c Hello.cpp -o Hello.o
xcrun clang -target arm64e-apple-macos11 -nostdlib -fapple-kext \
  -Xlinker -kext -Xlinker -undefined -Xlinker dynamic_lookup \
  -Xlinker -fixup_chains \
  Hello.o -o Hello.kext/Contents/MacOS/Hello
```

`file` shows what was built:

```console
$ file Hello.kext/Contents/MacOS/Hello
Hello.kext/Contents/MacOS/Hello: Mach-O 64-bit kext bundle arm64e
```

A kext that Xcode builds has its `kmod_info` generated for it. Built this
way, the kext declares it itself, with `KMOD_EXPLICIT_DECL` from
`<mach/kmod.h>`, as [`Hello.cpp`](../../examples/hello-kext/Hello.cpp)
does.

### The Info.plist

The Info.plist must be an XML property list; Ambrosia doesn't read binary
ones. Ambrosia needs two of its keys:

- `CFBundleIdentifier`: the kext's identifier, which `kmutil` and
  Ambrosia's log show.
- `CFBundleExecutable`: the executable's name in `Contents/MacOS/`.
  Ambrosia places only kexts that have an executable. A kext whose
  Info.plist has no `CFBundleExecutable` is read but not placed, so the
  kernel doesn't see it.

The kernel reads the rest, as it does for any kext:

- `CFBundlePackageType`, `KEXT`, and `CFBundleVersion`, the version
  `kmutil` shows.
- `OSBundleLibraries`: the KPIs the kext links against, such as
  `com.apple.kpi.iokit` and `com.apple.kpi.libkern`.
- `IOKitPersonalities`: what the kernel matches the kext's IOService
  classes to. The example's one personality matches `IOResources`, which
  every system has, with its own `IOMatchCategory`, so that it doesn't
  compete with other drivers there.

## Bundling it

The configuration names each kext by its bundle's directory in the
archive. The example's
[`config.plist`](../../examples/hello-kext/config.plist) adds one:

```xml
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
```

`ambrosia-bundle` puts the configuration and the kext in one archive. It
is a host program in
[`crates/ambrosia-bundle`](../../crates/ambrosia-bundle/README.md), which
this command, run in the repository, builds as
`target/release/ambrosia-bundle` ([building](../building.md#the-bundle-tool)):

```sh
cargo build --release -p ambrosia-bundle
```

Put it on your `PATH`, or run it from there:

```sh
ambrosia-bundle create --config config.plist --kext Hello.kext --output hello.cpio
```

`make hello-kext-bundle` does all of this for the example: it builds the
kext into `target/hello-kext/Hello.kext` and the bundle as
`target/hello-kext.cpio`.

`ambrosia-bundle` checks the bundle with the firmware's own readers before
it writes it. It refuses a kext that `config.plist` doesn't name, and one
whose executable is not an arm64e kext bundle the firmware can place, and
says what to change. It writes `config.plist` first, then the kext's files
under `Kexts/Hello.kext/`, as `ambrosia-bundle list` shows, with each
file's size in bytes:

```console
$ ambrosia-bundle list hello.cpio
  347  config.plist
 1256  Kexts/Hello.kext/Contents/Info.plist
99096  Kexts/Hello.kext/Contents/MacOS/Hello
3 files, 101768 bytes
```

To add several kexts, list each under `Kernel` > `Add` in the order the
kernel should consider them. A kext may import symbols from another kext
of the same configuration, such as a plugin from the base kext it extends.

## Booting with it

Give the bundle to the guest with `-b`
([running a guest](../running.md)):

```sh
scripts/run-guest.sh -e "$ECID" -b hello.cpio path/to/vm
```

Ambrosia's log on the serial port says what it placed:

```text
ambrosia: configuration: 101768 bytes, 1 kexts, 0 patches, graphics driver none
...
ambrosia: AuxKC: 131072 bytes, 1 kext(s), 318 fixups, BootKCID <data>
ambrosia: AuxKC: kext ai.aneesiqbal.ambrosia.example.hello at 0x4000 (slid 0xfffffe001493c000), code 0x10000, kmod 0x10000
ambrosia: AuxKC placed at phys 0x100938000, virt 0xfffffe0014938000, 0x20000 bytes, 1 kext(s)
```

The addresses change from boot to boot.

## Checking the guest

Once the guest answers SSH, `kmutil` lists the kext:

```console
$ kmutil showloaded --bundle-identifier ai.aneesiqbal.ambrosia.example.hello
No variant specified, falling back to release
Index Refs Address            Size       Wired      Name (Version) UUID <Linked Against>
  115    0 0                  0x10000    0x10000    ai.aneesiqbal.ambrosia.example.hello (1.0.0) 03FA7678-37EB-3D4E-B5AE-3E4FA181C103 <4 3>
```

The I/O Registry has the example's service, with the property its `start`
set:

```console
$ ioreg -r -c ai_aneesiqbal_ambrosia_example_Hello
+-o ai_aneesiqbal_ambrosia_example_Hello  <class ai_aneesiqbal_ambrosia_example_Hello, id 0x1000002b6, registered, matched, active, busy 0 (0 ms), retain 5>
    {
      "IOProbeScore" = 0
      "CFBundleIdentifier" = "ai.aneesiqbal.ambrosia.example.hello"
      "IOMatchCategory" = "ai_aneesiqbal_ambrosia_example_Hello"
      "IOClass" = "ai_aneesiqbal_ambrosia_example_Hello"
      "IOProviderClass" = "IOResources"
      "CFBundleIdentifierKernel" = "ai.aneesiqbal.ambrosia.example.hello"
      "Greeting" = "Hello, world"
      "IOResourceMatch" = "IOKit"
    }
```

The kext's two `IOLog` lines are in the kernel's log:

```console
$ /usr/bin/log show --last boot --style syslog --predicate 'process == "kernel" AND eventMessage CONTAINS "ai.aneesiqbal.ambrosia.example.hello"'
Timestamp                       (process)[PID]
<time>  localhost kernel[0]: (kernel) ai.aneesiqbal.ambrosia.example.hello: loaded
<time>  localhost kernel[0]: (kernel) ai.aneesiqbal.ambrosia.example.hello: Hello, world
```

Each line starts with its date and time, shown here as `<time>`. The log
gives `kernel` as the lines' sender, not the kext, so the predicate matches
on the text the kext logs. zsh, the default shell on macOS, has a `log`
builtin of its own, so give the command's full path.

SIP is still enabled:

```console
$ csrutil status
System Integrity Protection status: enabled.
```

## Limits

- The whole bundle may be up to 512 MiB
  ([the bundle](../configuration.md#the-bundle)).
- The Auxiliary kernel collection and the guest's trust caches share the
  space below the kernel collection, in the first 32 MiB of RAM
  ([the memory layout](../boot-process.md#the-memory-layout)). Kexts too
  large to fit there stop the boot with
  `boot failed: the layout doesn't fit in RAM, or the kexts and trust caches don't fit below the kernel collection`.

## When the boot stops

A kext Ambrosia can't place stops the boot before the kernel runs, with
one of these lines on the serial port:

- `bad configuration: config.plist: Kernel.Add names Kexts/Hello.kext, but the bundle has no Kexts/Hello.kext/Contents/Info.plist`:
  the bundle has no Info.plist at that path.
- `bad configuration: the bundle has no Kexts/Hello.kext/Contents/MacOS/Hello, which a kext's CFBundleExecutable names`:
  the bundle has no executable at that path.
- `bad configuration: Kexts/Hello.kext/Contents/Info.plist doesn't parse as a property list: ...`:
  the Info.plist is not an XML property list.
- `bad configuration: Kexts/Hello.kext/Contents/Info.plist: CFBundleIdentifier is missing or has the wrong type`:
  the Info.plist has no `CFBundleIdentifier`, or it is not a string. The
  same error names `CFBundleExecutable` when that is not a string.
- `boot failed: kext <id>: the executable is not a 64-bit little-endian Mach-O`
  or `boot failed: kext <id>: the executable is not a kext, an MH_KEXT_BUNDLE`:
  the executable is not a 64-bit Mach-O, or not an `MH_KEXT_BUNDLE`. `<id>`
  is the kext's `CFBundleIdentifier`.
- `boot failed: kext <id>: the executable has no chained fixups`: link it
  with `-fixup_chains`.
- `boot failed: kext <id>: the executable has no _kmod_info symbol`.
- `boot failed: kext <id>: a load command, table or name in the executable runs past its end`,
  `boot failed: kext <id>: a load command in the executable is too short for what it holds`,
  `boot failed: kext <id>: the executable's header and load commands are not in a segment at file offset 0`,
  or `boot failed: kext <id>: a segment's contents run past the end of the executable or are larger than the segment, or the segment ends past the top of the address space`:
  the executable is cut short or malformed.
- `boot failed: kext <id>: a page of the executable's chained fixups has more than one chain start`
  or `boot failed: kext <id>: the executable's chained fixups have <form>, which Ambrosia doesn't read`:
  the chained fixups take a form Ambrosia doesn't read. Build the kext as
  `build.sh` does: `-fixup_chains` gives 4-byte imports with plain names
  and `DYLD_CHAINED_PTR_ARM64E_KERNEL` pointers, with no addends.
- `boot failed: kext <id>: it has a segment named <name>; Ambrosia places only __TEXT, __TEXT_EXEC, __DATA_CONST, __OBJC_CONST and __DATA`:
  the kext has a segment other than those above.
- `boot failed: kext <id>: it imports <symbol>, which neither the kernel collection nor the other kexts define`:
  the kext imports a symbol that neither the kernel collection nor the
  configuration's other kexts define.

`ambrosia-bundle create` and `check` find all but the last of these on the
host, before the boot. The last shows only at boot, since it depends on the
guest's kernel collection.
