# Ambrosia documentation

## Using Ambrosia

- [Building](building.md): the toolchain, the Makefile, reproducible
  builds and releases.
- [Running a guest](running.md): the QEMU command line, the disks, the
  configuration bundle and the boot log.
- [Configuration](configuration.md): the bundle and every key of
  `config.plist`.
- [The machine](machine.md): what the VMM must give, and why macOS needs
  it.
- [The boot process](boot-process.md): from reset to the kernel, step by
  step.
- [Graphics drivers](graphics-drivers.md): the driver module interface.

## How-to guides

- [Installing a guest and finding its ECID](how-to/install-a-guest.md)
- [Running under HVF](how-to/run-under-hvf.md)
- [Running under TCG](how-to/run-under-tcg.md)
- [Building a bundle](how-to/build-a-bundle.md)
- [Adding a kext](how-to/add-a-kext.md)
- [Writing a graphics driver module](how-to/write-a-graphics-driver.md)
- [Reading the boot log and troubleshooting a boot](how-to/troubleshoot-a-boot.md)

## Internals

- [How Ambrosia replaces iBoot](internals/replacing-iboot.md): the kernel
  collection, the Auxiliary KC it builds for kexts, the device tree, the
  boot arguments, the PAC spins and the state at kernel entry.
- [The handover to injected code](internals/handover.md): what
  `/chosen/ambrosia` records about the kexts Ambrosia placed.
- [Lessons learned](internals/lessons-learned.md): what booting macOS
  without Apple's firmware took to find out, and why each thing is so.

## The code

Each crate has a README of its own:
[`crates/ambrosia`](../crates/ambrosia/README.md), the core library;
[`crates/firmware`](../crates/firmware/README.md), the image;
[`crates/ambrosia-bundle`](../crates/ambrosia-bundle/README.md), the bundle
tool; and [`drivers/bochs-display`](../drivers/bochs-display/README.md),
the example driver module.
[`examples/hello-kext`](../examples/hello-kext/README.md) is a minimal kext
to inject.
