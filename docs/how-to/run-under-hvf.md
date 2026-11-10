# Running under HVF

On an Apple silicon Mac, QEMU runs the guest under Hypervisor.framework
(HVF), the fastest way to boot it: a guest answers SSH about 10 to 25
seconds after QEMU starts. Only Hypervisor.framework's public API is used,
so the host keeps System Integrity Protection on.

## What you need

- An Apple silicon Mac.
- A QEMU built for `aarch64-softmmu` with HVF, which is built in by default
  on macOS, and a crypto library such as gnutls, whose `vmapple` machine
  gives what [the machine](../machine.md#what-the-machine-must-give) lists.
  Under HVF two of those matter most: QEMU must start each secondary CPU on
  the boot CPU's pointer authentication keys, and it must decode the MMIO
  accesses macOS makes without a syndrome.
- The image, `target/ambrosia.bin` from `make`.
- An installed guest and its ECID
  ([installing a guest](install-a-guest.md)).

## Boot

```sh
scripts/run-guest.sh -e "$ECID" -f target/ambrosia.bin \
    -q /path/to/qemu-system-aarch64 path/to/vm
```

The script runs QEMU with `-accel hvf,kernel-irqchip=on`, which uses HVF's
in-kernel interrupt controller, and with 8 GiB and 4 CPUs unless `-m` and
`-c` say otherwise. The serial log goes to the terminal, or to a file with
`-s`. Host port 10022 is forwarded to the guest's SSH port; `-p` picks
another, so that two guests can run at once.

When Ambrosia's log ends with `entering the kernel at ...`, macOS is
starting. Wait for SSH, then check SIP:

```sh
ssh -p 10022 user@127.0.0.1 csrutil status
```

## What differs from Virtualization.framework

- **Pointer authentication.** A `vmapple` kernel asks the hypervisor to
  manage its pointer authentication keys. HVF's public API refuses those
  calls, and only a private mode, which needs the host's SIP off,
  implements them. Ambrosia makes the kernel run without them instead
  ([the PAC hypercalls](../machine.md#the-pac-hypercalls)), so the kernel
  and every process share one set of keys.
- **The display.** The guest has no display macOS drives, so it never
  reaches its desktop. It runs its services and answers SSH.
- **The CPU.** The guest gets the host's CPU and its features.

## Problems you may meet

- `hvf: unhandled ISV=0 data abort`: QEMU took an MMIO access without a
  syndrome it can't decode. Either the QEMU lacks that decoding
  ([the machine](../machine.md#mmio-accesses-without-a-syndrome-under-hvf)),
  or the firmware isn't Ambrosia: AVPBooter stops this way on a machine
  with RAM at 4 GiB, since its stack is in the PCIe window there.
- A macOS 26 guest hangs as it mounts its root volume: the QEMU was built
  without a crypto library, which the machine's AES engine needs.

[Reading the boot log](troubleshoot-a-boot.md) covers the rest.
