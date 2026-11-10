# Configuration

Ambrosia reads its configuration from a bundle that QEMU puts in RAM. The
bundle is optional, and a guest boots without one: Ambrosia then gives the
kernel an empty command line, adds no kexts, applies no patches and starts
no graphics driver module.

## The bundle

A bundle is a cpio archive in the "newc" format whose first file is
`config.plist`. The other files are whatever the configuration names, by
their paths in the archive:

```text
config.plist
Kexts/Hello.kext/Contents/Info.plist
Kexts/Hello.kext/Contents/MacOS/Hello
Drivers/display.elf
```

`ambrosia-bundle` builds one from a `config.plist`, the kexts and the
driver modules, and checks it with the same parsers the firmware uses:

```sh
ambrosia-bundle create --config config.plist \
    --kext Hello.kext --driver display.elf --output bundle.cpio
ambrosia-bundle check bundle.cpio
ambrosia-bundle list bundle.cpio
```

It puts each kext at `Kexts/<Name>.kext/` and each driver module at
`Drivers/<file>`, so `config.plist` names them by those paths.
[Building a bundle](how-to/build-a-bundle.md) covers the tool.

QEMU's generic loader puts the bundle at 0x120000000, 512 MiB into RAM:

```sh
-device loader,file=bundle.cpio,addr=0x120000000,force-raw=on
```

Ambrosia looks for a newc header there. If there is none, it boots without
a configuration. A bundle may be up to 512 MiB, the space between that
address and Ambrosia's heap. Ambrosia keeps only the archive's regular
files: directories, links and other members are skipped, and a leading
`./` is dropped from each path.

`make bochs-display-bundle` builds an example, `target/bochs-display.cpio`,
whose `config.plist` starts the example graphics driver module
([building](building.md#bochs-display-and-bochs-display-bundle)).

## config.plist

An XML property list whose top level is a dictionary. Binary property lists
are not read. Every key is optional, and keys not listed here are ignored.
Integers may be decimal or `0x` hexadecimal, and `<data>` is base64.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>BootArgs</key>
  <string>serial=3 -v</string>
  <key>Kernel</key>
  <dict>
    <key>Add</key>
    <array>
      <dict>
        <key>BundlePath</key>
        <string>Kexts/Hello.kext</string>
      </dict>
    </array>
    <key>Patch</key>
    <array>
      <dict>
        <key>Identifier</key>
        <string>com.apple.kernel</string>
        <key>Find</key>
        <data>RGFyd2luIEtlcm5lbCBWZXJzaW9uIA==</data>
        <key>Replace</key>
        <data>QW1icm9zaWEtcGF0Y2hlZCBrZXJuIA==</data>
        <key>Count</key>
        <integer>1</integer>
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

This configuration puts the kernel's log on the serial port, adds the kext
the bundle holds at `Kexts/Hello.kext`, replaces the first
"Darwin Kernel Version " in the kernel with "Ambrosia-patched kern ", so
that the kernel's first log line starts with it, and starts the
bochs-display module, which the bundle must hold at
`Drivers/bochs-display.elf`.

### BootArgs

A string: the kernel's command line. The default is empty, and a guest
needs none. Ambrosia writes it into the boot arguments' 608-byte command
line field, cut to 607 bytes if longer.

For debugging, `serial=3 -v` puts the kernel's log on the UART, after
Ambrosia's own.

### Kernel > Add

An array of kexts to add, each a dictionary:

- `BundlePath`, a string, required: the kext bundle's directory in the
  archive, such as `Kexts/Hello.kext`. A trailing `/` is ignored. The
  bundle must hold `Contents/Info.plist` there, an XML property list with a
  `CFBundleIdentifier`. If the Info.plist names a `CFBundleExecutable`, the
  bundle must hold `Contents/MacOS/<name>` too.
- `Enabled`, a boolean, default true. An entry set to false is skipped.

Ambrosia places every enabled kext that has an executable in one Auxiliary
kernel collection that it builds in memory, and the kernel loads it and
starts the kexts with SIP on and nothing done in the guest. No kext
signature or trust-cache entry is needed: the guest runs with developer
mode force-enabled, as the `vmapple` machine reports it. A placed kext's
`IOKitPersonalities` and `OSBundleLibraries` carry through, so the kernel
matches, links and starts it as it would any Auxiliary-KC kext. A kext
with no executable is read but not placed.

A kext's executable must be an arm64e `MH_KEXT_BUNDLE` with chained fixups
(linked with `-fixup_chains`) and a `_kmod_info` symbol. Its `__TEXT`
segment must come first in the file, and its other segments with contents,
but `__LINKEDIT`, must be `__TEXT_EXEC`, `__DATA_CONST`, `__OBJC_CONST` or
`__DATA`.
A kext built against the macOS SDK's Kernel.framework, with no KDK, can
meet all of this. [Adding a kext](how-to/add-a-kext.md) walks through
building one, and
[`examples/hello-kext`](../examples/hello-kext/README.md) is a minimal kext
to start from.

List the kexts in the order the kernel should consider them. An imported
symbol resolves against every member of the kernel collection, the kernel
and its kexts, and then against the other kexts in the same configuration,
so a configuration can add a base kext together with the plugins that
import its symbols, the way OpenCore and Lilu load on x86-64.

Each placed kext is recorded in the device tree for code that runs in the
kernel to find ([the handover](internals/handover.md)). A kext Ambrosia
can't place stops the boot with `boot failed: kext <id>: <why>`, where
`<id>` is the kext's `CFBundleIdentifier` and `<why>` is one of these:

- The executable isn't a kext Ambrosia can read:
  - `the executable is not a 64-bit little-endian Mach-O`
  - `the executable is not a kext, an MH_KEXT_BUNDLE`
  - `a load command, table or name in the executable runs past its end`
  - `a load command in the executable is too short for what it holds`
  - `the executable's header and load commands are not in a segment at file offset 0`
  - `a segment's contents run past the end of the executable or are larger than the segment, or the segment ends past the top of the address space`
  - `the executable has no chained fixups`
  - `the executable has no _kmod_info symbol`
- The executable's chained fixups take a form Ambrosia doesn't read. It
  reads what the static linker writes for a kext linked with
  `-fixup_chains`: 4-byte imports with plain names,
  `DYLD_CHAINED_PTR_ARM64E_KERNEL` pointers, and no addends.
  - `a page of the executable's chained fixups has more than one chain start`
  - `the executable's chained fixups have <form>, which Ambrosia doesn't read`
- `it imports <symbol>, which neither the kernel collection nor the other kexts define`:
  an imported symbol that neither the kernel collection nor the other
  configured kexts define.
- `it has a segment named <name>; Ambrosia places only __TEXT, __TEXT_EXEC, __DATA_CONST, __OBJC_CONST and __DATA`.
- Another reason Ambrosia can't lay the kext out, such as
  `its first segment with contents is not at file offset 0` or
  `its _kmod_info is not wholly inside a segment Ambrosia places`.

### Kernel > Patch

An array of find-and-replace patches, each on one entry of the kernel
collection, each a dictionary:

- `Identifier`, a string, required: the bundle identifier of the entry to
  patch, `com.apple.kernel` for the kernel itself.
- `Find`, data, required: the bytes to find. It must not be empty.
- `Replace`, data, required: the bytes to put in their place, as many as
  `Find` has.
- `Count`, an integer, default 0: how many matches to replace, counting
  from the start of the entry. 0 replaces every match.
- `Enabled`, a boolean, default true. A patch set to false is skipped.

Patches apply in the order they are listed, after the kernel collection is
unpacked in place and its PAC spins are made `nop`s, and before the kernel
runs. A patch searches each segment of its entry in turn, all but
`__LINKEDIT`, which every entry shares, and replaces matches that don't
overlap. A match can't span two segments. The bytes are the collection's
as it is stored, before the kernel applies its chained fixups, so a pointer
in a data segment is in its chained fixup form, not its final address.

Each patch logs how many matches it replaced:

```text
ambrosia: patch for com.apple.kernel: 1 replaced
```

A patch that matches nothing doesn't stop the boot. A patch whose
`Identifier` names no entry does, with
`boot failed: Kernel.Patch names <id>, which is not an entry of the kernel collection`.

### Kernel > ExportVtableDiscriminators

An array of C++ vtable symbol strings, such as
`__ZTV25IOMobileFramebufferLegacy`. For each, Ambrosia reads the vtable's
per-slot pointer-authentication discriminators from the kernel collection
and records them in the device tree, for injected code that defines a
subclass of that class and must sign its inherited vtable slots.
Ambrosia names no class itself: the configuration names the symbols.
[The handover](internals/handover.md#vtable-discriminators) gives the
records' layout.

A slot's discriminator for a method the subclass implements is the one in
the vtable of the most-base class that has that slot. A class that declares
an inherited method pure again signs that slot of its own vtable with a
different discriminator, which callers do not use. So list the base classes'
vtables too, for example `__ZTV19IOMobileFramebuffer` beside
`__ZTV25IOMobileFramebufferLegacy`, and take each slot from the most-base
table that has it.

A symbol the kernel collection does not define stops the boot with
`boot failed: Kernel.ExportVtableDiscriminators names <symbol>, which the kernel collection doesn't define`.

### Graphics > Driver

A string: the path in the bundle of a graphics driver module to start, an
ELF file in the format [graphics drivers](graphics-drivers.md) describes.
Ambrosia loads it and calls it once, after it reads the configuration and
before it reads the guest's disks. The module sets up a display, on which
Ambrosia then shows its log, and which it hands to the kernel. Without a
driver the log goes only to the UART.

If the module finds no device it drives, the boot goes on without a
console. If the loader refuses the file, the boot stops with
`bad graphics driver <path>: ...`.

### Graphics > DisplayNode

A string: the name of a device tree node that Ambrosia adds under
`/arm-io` for the display the driver module sets up, for a kernel display
driver to attach to. Set it only when the configuration also adds that
driver, as a kext under `Kernel` > `Add`.

```xml
<key>Graphics</key>
<dict>
  <key>Driver</key>
  <string>Drivers/example.elf</string>
  <key>DisplayNode</key>
  <string>example-display0</string>
</dict>
```

The node has two properties: `name`, the string, and `device_type`,
`display-subsystem`. macOS's IOMobileFramebuffer framework counts the
`display-subsystem` nodes under `/arm-io` the first time a process asks
for a display. A process that counts none never looks for a framebuffer
driver, and one that counts some waits up to 30 seconds for a driver to
appear. WindowServer asks before any added kext starts, so a kext's own
node comes too late for it. With the node in the tree from the start,
WindowServer finds the display once the kext attaches to the node.

Ambrosia adds the node only when the module drives a display, and logs it:

```text
ambrosia: display node /arm-io/example-display0, a display-subsystem
```

The name must not be empty or hold a `/`, or the boot stops with
`bad configuration: config.plist: Graphics.DisplayNode is empty or holds a /`.
A name `/arm-io` already has stops the boot with
`boot failed: /arm-io already has a node named as Graphics.DisplayNode`.

## Errors

A bundle or configuration Ambrosia can't use stops the boot before the
disks are read, with one of these lines. `ambrosia-bundle check` finds
the same problems on the host, with the firmware's own parsers.

- `bad configuration bundle: the archive's first file is not config.plist`.
- `bad configuration bundle: the archive member at byte <n> has no newc header`,
  `... runs past the end of the bundle's space` or
  `... has a path that is not UTF-8`: the archive member at byte offset `<n>`
  is not one Ambrosia reads.
- `bad configuration: <file> doesn't parse as a property list: <why> at byte <n>`:
  `config.plist` or a kext's Info.plist, `<file>`, is not an XML property
  list Ambrosia reads. Parsing stopped at byte offset `<n>`.
- `bad configuration: config.plist: its top level is not a dictionary`.
- `bad configuration: <file>: <key> is missing or has the wrong type`: a
  required key is missing, or a key has the wrong type. `<key>` is the
  key's path, such as `Kernel.Add[0].BundlePath`, and `<file>` is
  `config.plist` or a kext's Info.plist, such as
  `Kexts/Hello.kext/Contents/Info.plist` for its `CFBundleIdentifier`.
- `bad configuration: config.plist: Kernel.Patch[<i>]: Find is empty or not as long as Replace`.
- `bad configuration: config.plist: Graphics.DisplayNode is empty or holds a /`.
- `bad configuration: config.plist: Kernel.Add names <path>, but the bundle has no <path>/Contents/Info.plist`:
  the kext's `BundlePath` is not a kext in the bundle.
- `bad configuration: the bundle has no <path>, which a kext's CFBundleExecutable names`:
  a kext's executable is not at `Contents/MacOS/<name>` in its bundle.
- `bad configuration: config.plist: Graphics.Driver names <path>, but the bundle has no such file`.
