# Running a guest

Ambrosia boots an installed macOS guest on QEMU's `vmapple` machine. QEMU
loads it with `-bios`, in place of AVPBooter, and gives it the guest's
disks.

You need:

- a QEMU whose `vmapple` machine gives what [the machine](machine.md)
  lists, built with HVF on an Apple silicon Mac or with TCG on any host;
- the image: `target/ambrosia.bin` from `make` ([building](building.md)),
  or `ambrosia.bin` from a release's zip;
- the guest's files: its disk image, `disk.img`, and its auxiliary
  storage, `aux.img`;
- the guest's ECID.

[Installing a guest](how-to/install-a-guest.md) shows how to get the files
and the ECID. [Running under HVF](how-to/run-under-hvf.md) and
[running under TCG](how-to/run-under-tcg.md) cover each accelerator.

## With scripts/run-guest.sh

`scripts/run-guest.sh` builds the whole command line:

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin path/to/vm
```

`path/to/vm` is the directory that holds `disk.img` and `aux.img`. On its
first run the script writes `aux.img.trimmed` there ([the
disks](#the-disks)), and the guest keeps its NVRAM in that copy from then
on. Its options:

- `-e ECID`: the guest's ECID, in decimal or `0x` hexadecimal. Required.
- `-f FILE`: the firmware image. The default is `target/ambrosia.bin`.
- `-b FILE`: a [configuration bundle](#the-configuration-bundle) to load.
- `-q FILE`: the QEMU to run. The default is `$QEMU`, or else
  `qemu-system-aarch64`.
- `-t`: run under TCG instead of HVF.
- `-m SIZE`: memory. The default is `8G`.
- `-c COUNT`: CPUs. The default is 4.
- `-p PORT`: the host port forwarded to the guest's SSH port. The default
  is 10022.
- `-s FILE`: write the serial log to `FILE`. By default it goes to the
  terminal.

Arguments after `--` go to QEMU as they are. `scripts/run-guest.sh -h`
prints the options.

## The command line

This is the command the script runs, for use with another launcher:

```sh
qemu-system-aarch64 -accel hvf,kernel-irqchip=on \
  -M vmapple,uuid=$ECID -m 8192M -smp 4 -bios ambrosia.bin \
  -drive file=aux.img.trimmed,if=pflash,format=raw \
  -drive file=disk.img,if=pflash,format=raw \
  -drive file=aux.img.trimmed,if=none,id=aux,format=raw \
  -drive file=disk.img,if=none,id=root,format=raw \
  -device vmapple-virtio-blk-pci,variant=aux,drive=aux \
  -device vmapple-virtio-blk-pci,variant=root,drive=root \
  -netdev user,id=net0,ipv6=off,hostfwd=tcp:127.0.0.1:10022-:22 \
  -device virtio-net-pci,netdev=net0,romfile= \
  -display none -serial file:serial.log
```

### The accelerator

`-accel hvf,kernel-irqchip=on` runs the guest under Hypervisor.framework,
with its in-kernel interrupt controller. Under TCG the line is
`-accel tcg,thread=multi` instead, and nothing else changes.

### The machine

`-M vmapple,uuid=$ECID` picks the machine and gives it the guest's ECID.
`-m` and `-smp` give its RAM and CPUs. The machine puts RAM at 4 GiB, and
Ambrosia gives macOS the CPU count the machine reports. The tested guests
boot with 8 GiB and 4 CPUs.

### The ECID

The machine's `uuid` property is the guest's ECID, a 64-bit number, not a
UUID. QEMU takes it in decimal or in hexadecimal with a `0x` prefix.
Ambrosia reads it from the machine's config block, logs it, and gives it to
macOS in the device tree as `unique-chip-id`. Give the ECID the guest was
installed with. [Installing a guest](how-to/install-a-guest.md#find-the-ecid)
shows where to find it.

### The disks

A guest has two disks, and the command line gives each one twice.

- The two `if=pflash` drives are what the machine's firmware disk
  interface reads, and what Ambrosia reads the boot objects and NVRAM
  from. Their order matters: the first is the aux disk, the second the
  root disk.
- The two `if=none` drives, attached as `vmapple-virtio-blk-pci` with
  `variant=aux` and `variant=root`, are the disks macOS itself sees.

Both are raw images (`format=raw`). `aux.img` as Virtualization.framework
writes it starts with a 16 KiB block the machine does not expect. QEMU
takes it without that block:

```sh
dd if=aux.img of=aux.img.trimmed bs=16384 skip=1
```

The trimmed copy holds NVRAM, which the guest writes as it runs. On the
tested guests NVRAM is 1 MiB at offset 0xA00000 of it, where the guest's
device tree template places it.

To leave a guest's files as they are, boot a clone of them: `cp -c` makes
one on APFS, and `cp --reflink=always` on Btrfs or XFS.

### The network

`-netdev user,...,hostfwd=tcp:127.0.0.1:10022-:22` with a `virtio-net-pci`
device gives the guest a network, and forwards host port 10022 to its SSH
port. The guest answers there if Remote Login is on in it. The device's
`romfile=` leaves off its PXE boot ROM, which a disk boot never loads, so a
QEMU built without that ROM still starts.

### The display

`-display none`: the machine has no display that macOS drives. The guest
boots and answers SSH, but never reaches its desktop: WindowServer finds
no Metal device, and the login window waits for it. A
[graphics driver module](graphics-drivers.md) can show Ambrosia's log and
the kernel's progress bar on a PCI display device, such as QEMU's
`bochs-display`.

### The serial log

`-serial file:serial.log` writes the machine's UART to a file.
[The boot log](#the-boot-log) says what is in it.

## The configuration bundle

A configuration bundle gives the kernel boot arguments, adds kexts, patches
the kernel collection, or names a graphics driver module to start. QEMU's
generic loader puts it in RAM before the firmware runs:

```sh
-device loader,file=bundle.cpio,addr=0x120000000,force-raw=on
```

0x120000000 is 512 MiB into RAM, where Ambrosia looks for a bundle.
`force-raw=on` makes the loader copy the file as it is rather than parse it
as an executable. A bundle may be up to 512 MiB. With none, Ambrosia boots
with no boot arguments, kexts, patches or driver module.
[Configuration](configuration.md) describes the format, and
[building a bundle](how-to/build-a-bundle.md) shows how to make one with
`ambrosia-bundle`.

## The boot log

Ambrosia writes its log to the machine's UART, a PL011 at 0x20010000, each
line starting with `ambrosia: `. The log ends with
`entering the kernel at ...`. After that the serial port stays quiet:
without boot arguments macOS logs nothing there.

The boot has worked when the guest answers SSH, and `csrutil status` in it
reports System Integrity Protection enabled. Under HVF that takes about 10
to 25 seconds from QEMU's start, and under TCG with 4 vCPUs one to two and
a half minutes.

[Reading the boot log](how-to/troubleshoot-a-boot.md) explains each line,
each error that stops the boot, and what to do when a guest doesn't come
up.

## Which guests boot

The [README](../README.md#what-works) lists the macOS builds that boot.
Two limits apply to any build:

- macOS 11 can't be a `vmapple` guest: no macOS 11 restore image has a
  build for the machine.
- Under TCG, 14.6.1's kernel panics as it starts on a CPU with SME
  ([the machine](machine.md#no-sme-under-tcg)), so its guests need a
  machine whose `max` CPU has SME off.
