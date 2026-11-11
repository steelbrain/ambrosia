# Reading the boot log and troubleshooting a boot

Ambrosia logs each step of the boot to the machine's UART.
`scripts/run-guest.sh` prints it to the terminal, or writes it to a file
with `-s FILE`. This page explains what a normal log says, each error that
stops the boot, and what to look at when a guest doesn't come up.

## A normal boot

A plain 27.0.1 guest's log, with the guest's identifiers replaced:

```text
ambrosia: Ambrosia 0.1.1
ambrosia: 4 CPUs, RAM 0x100000000-0x300000000, ECID 0x<ECID>
ambrosia: macOS <volume group UUID>, boot objects <manifest hash>
ambrosia: NVRAM bank 1 of 2, generation 260
ambrosia: NVRAM has no RTC offset: set it to zero
ambrosia: kernel collection: 216 entries, 0x4dac000 bytes at 0x100a70000
ambrosia: 19 PAC spins made nops
ambrosia: physBase 0x100000000 virtBase 0xfffffe0020000000 top 0x1058b0000 memSize 0x1ffb38000 slide 0x19a6c000
ambrosia: entering the kernel at 0x103920480
```

Line by line:

1. The firmware's version.
2. What the machine's config block reports: the CPU count, the RAM range
   and the ECID.
3. The macOS install found on the root disk, by its volume group, and the
   directory of its boot objects, named for its boot manifest's SHA-384.
4. The current NVRAM bank on the aux disk.
5. Only when NVRAM has no clock offset for `avp-rtc`: Ambrosia adds the
   offset for zero
   ([boot process](../boot-process.md#nvram-and-the-clock)).
6. The kernel collection, unpacked and placed.
7. How many of the kernel's PAC spins were made `nop`s. The 12.6 and 13.6
   kernels have none, and those of 14.6.1 to 27.0.1 have 19 each.
8. The memory layout written into the boot arguments, and the kernel's
   slide.
9. The jump into the kernel.

Where the boot directory has no `System/Library/Caches/apticket.der`, the
boot manifest comes from the Preboot volume's `restore` directory, and a
line after the third says which file it was:
`boot manifest: /<group>/restore/<file>, whose SHA-384 names the boot directory`
([boot process](../boot-process.md#reading-the-boot-objects)).

A configuration bundle adds lines of its own:

- `configuration: <bytes> bytes, <n> kexts, <n> patches, graphics driver <path>`,
  after the second line;
- the graphics driver module's lines, and the early console's
  ([graphics drivers](../graphics-drivers.md#a-boot-with-the-example));
- `patch for <identifier>: <n> replaced`, one for each patch;
- `AuxKC: ...` lines for the kexts it placed
  ([placing the kexts](../boot-process.md#placing-the-kexts));
- `vtable-disc: ...` and `display node ...`, when the configuration asks
  for them.

## After the log ends

After `entering the kernel at ...` the serial port stays quiet: without
boot arguments macOS logs nothing there. The boot has worked when the guest
answers SSH, and `csrutil status` in it reports System Integrity Protection
enabled:

```sh
ssh -p 10022 user@127.0.0.1 csrutil status
```

Under HVF that takes about 10 to 25 seconds from QEMU's start, and under
TCG one to two and a half minutes.

## Seeing the kernel's log

To see what macOS does after Ambrosia, boot with a bundle whose `BootArgs`
puts the kernel's log on the serial port:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>BootArgs</key>
  <string>serial=3 -v</string>
</dict>
</plist>
```

```sh
ambrosia-bundle create --config config.plist --output debug.cpio
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin -b debug.cpio \
    -s serial.log path/to/vm
```

The kernel's log follows Ambrosia's in `serial.log`, panics included.

## When the boot stops

On an error Ambrosia logs one line, and the boot CPU halts. Nothing else
runs, and QEMU keeps running until it is stopped.

**The configuration.** These come before any disk is read.
[Configuration](../configuration.md#errors) explains each, and
`ambrosia-bundle check` finds the same problems on the host.

- `bad configuration bundle: ...`: the archive can't be read, or its first
  file isn't `config.plist`.
- `bad configuration: ...`: `config.plist` or a kext's Info.plist can't be
  read, a key is wrong, or a file it names isn't in the bundle. The error
  names the key's path or the file.
- `bad graphics driver <path>: ...`: the loader refused the driver module.

**The disks.** `can't read the boot objects: ...` means a disk couldn't be
read, or a partition, volume or file isn't where Ambrosia looks for it.

- `GPT: no header signature at LBA 1`: the root disk has no partition
  table. Check the order of the two `if=pflash` drives: the first is the aux
  disk, the second the root disk.
- `the disk has no single APFS partition`, or `... iBoot system container
  partition`: the root disk isn't a macOS disk, or holds more than one
  container of that kind. Ambrosia boots a disk with one macOS install.
- `the container has no single System volume`, or another role: the APFS
  container has no such volume, or more than one.
- `APFS: ...`: the APFS reader found something it can't read, such as a
  block that fails its checksum, or an encrypted or compressed file.
- `no NVRAM region: ...`: the guest's device tree template gives no NVRAM
  region on the aux disk.
- `the aux disk failed to read NVRAM: ...`: the aux disk couldn't be read
  where the template puts NVRAM. Check that the first `if=pflash` drive is
  the trimmed aux image, `aux.img.trimmed`.

**Laying out the kernel.** `boot failed: ...` means the boot objects were
read but couldn't be laid out:

- `Kernel.Patch names <id>, which is not an entry of the kernel collection`:
  a patch's `Identifier` names no entry of the kernel collection.
- `kext <id>: ...`: the configured kext whose `CFBundleIdentifier` is
  `<id>` can't be placed, for the reason the line gives
  ([Kernel > Add](../configuration.md#kernel--add)).
- `can't build the Auxiliary KC: ...`: the kexts can't be placed together,
  such as when their chained fixups don't fit.
- `Kernel.ExportVtableDiscriminators names <symbol>, which the kernel collection doesn't define`:
  an exported vtable symbol isn't in the kernel collection.
- `/arm-io already has a node named as Graphics.DisplayNode`: `/arm-io`
  already has a node by the `DisplayNode` name.
- `the layout doesn't fit in RAM, or the kexts and trust caches don't fit below the kernel collection`:
  give the guest more memory, or place fewer kexts.
- `the device tree template names no NVRAM region, or the aux disk's NVRAM region holds no valid bank`:
  the aux disk's NVRAM region has no valid bank.

**Faults.** `exception N: ESR ... ELR ... FAR ...`, then the registers,
means the firmware or a driver module took an exception.
[Reading an exception](../../crates/firmware/README.md#reading-an-exception)
shows how to find the code at `ELR`. Under TCG, a `FAR` of `0x30240040`
means the machine has no avp-rtc
([running under TCG](run-under-tcg.md#problems-you-may-meet)).
`panic: ...` means the firmware panicked, and says where.

## Checking that a kext started

When the configuration adds kexts, Ambrosia's log has an `AuxKC: kext ...`
line for each. Once the guest answers SSH, `kmutil` lists each one the
kernel loaded, and the kernel's log has the lines the kext wrote with
`IOLog`:

```sh
kmutil showloaded --bundle-identifier ai.aneesiqbal.ambrosia.example.hello
/usr/bin/log show --last boot --style syslog \
    --predicate 'process == "kernel" AND eventMessage CONTAINS "ai.aneesiqbal.ambrosia.example.hello"'
```

The log gives `kernel` as the sender of a kext's `IOLog` lines, so match on
the text the kext writes. Give `log` its full path: zsh, the default shell
on macOS, has a `log` builtin of its own. `dmesg` doesn't show the lines on
27.0.1. [Adding a kext](add-a-kext.md#checking-the-guest) shows the output
for the example kext.

## When the guest doesn't come up

The log ends with `entering the kernel at ...`, but SSH never answers.

- **The log starts again.** With no boot arguments a kernel panic shows
  only as a reset: the guest reboots, and Ambrosia's lines appear again.
  Boot with `serial=3 -v` ([above](#seeing-the-kernels-log)) to read the
  panic.
- **SSH is refused or times out.** Check that Remote Login is on in the
  guest, and that you connect to the port `-p` gave, 10022 by default.
- **The guest has no network on some boots.** A machine that offers
  virtio-net with a control queue leaves about one boot in ten of 26.6.2 or
  27.0.1 without a network
  ([the machine](../machine.md#virtio-net-without-its-control-queue)).
- **A macOS 27 guest hangs.** macOS 27 never starts a virtio device whose
  capabilities are linked from high offsets to low, so the machine's
  virtio devices must link them in ascending order
  ([the machine](../machine.md#the-rest)).
- **Only some CPUs come up, or the kernel panics on another CPU.** The VMM
  must start each secondary CPU on the boot CPU's pointer authentication
  keys ([the machine](../machine.md#secondary-cpus-on-the-boot-cpus-keys)).
- **Under TCG**, see
  [running under TCG](run-under-tcg.md#problems-you-may-meet) for SME and
  the DMA config doorbell.

## Known behavior

- **No desktop.** A guest runs its services and answers SSH, but
  WindowServer waits for a Metal device it has a driver for, so the login
  session never starts.
- **13.6's clock.** 13.6 has no `avp-rtc` driver, and its timed sets the
  clock back to the disk's last write on each boot, until it reaches a time
  server. Later builds keep calendar time through `avp-rtc`.
