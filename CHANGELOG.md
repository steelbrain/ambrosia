# Changelog

Every release of Ambrosia, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each
release's image is published with the release.

## Unreleased

- The graphics driver interface's rule for device registers is the one the
  CPU has: each access one load or store of one general-purpose register,
  with no writeback, which the CPU reports to the hypervisor with a
  syndrome. The docs said a volatile access was enough, but a compiler may
  make one a pre- or post-indexed load, which a VMM must decode itself:
  QEMU on KVM, without such a decoder, injects an abort, which stops the
  boot in the module. The example module makes its accesses in inline
  assembly, in `src/mmio.rs`, the firmware makes its own the same way, and
  the docs show it in C.
- macOS 27.2.1 (26B131) guests boot under HVF with SIP enabled, and with
  kexts added.
- A boot directory without `System/Library/Caches/apticket.der` boots:
  Ambrosia takes the boot manifest from the `apticket.*.im4m` in
  `/<group>/restore/` whose SHA-384 is the boot directory's name, and logs
  which file it read. The Preboot volume keeps a copy of the manifest
  there, and a disk built without macOS's installer, such as one macOS
  27.2.1 (26B131) guest image, can have only that copy.
- The boot arguments are written in both of XNU's layouts. The 12.6 and
  13.6 kernels read 736 bytes. The kernels from 14.6.1 on read 1152 bytes,
  with a 1024-byte command line, so their `bootFlags` and `memSizeActual`
  lie at 1136 and 1144. Ambrosia wrote only the shorter layout, and the
  27.2.1 kernel, which compares `memSizeActual` with `/chosen/dram-size`,
  read 0 and panicked with `inconsistent DRAM size reported by iBoot`.
  `memSizeActual` is now all of RAM, the value `/chosen/dram-size` has.
- A `Kernel` > `Patch` on `com.apple.kernel` applies. It stopped the boot
  with
  `the kernel collection can't be read: its entry point is in no segment`:
  reading the kernel member to patch it checked the member's own
  `LC_UNIXTHREAD`, whose address is in none of its segments in the
  collections of macOS 14.6.1, 15.6.1, 26.6.2 and 27.0.1. Only the
  collection's entry point is checked now.

## 0.1.1 - Automatic Release by CI

- CI builds and publishes each release. Pushing a tag `vX.Y.Z` builds the
  release on Linux and on macOS, checks that both machines built the same
  bytes, and creates the tag's GitHub release with this changelog's section
  as its notes and `ambrosia-X.Y.Z.zip` attached.
- Each release is one zip, `ambrosia-X.Y.Z.zip`, holding the image, its
  `SHA256SUMS`, the third-party license notices, the commit it was built
  from and Ambrosia's license texts. `scripts/internal/release-zip.sh`
  builds it, the same bytes on any host, so a downloaded zip can be checked
  by rebuilding it from its tag.

## 0.1.0 - Initial release

- Boots arm64 macOS on QEMU's `vmapple` machine in place of AVPBooter, with
  iBoot never running: guests of 12.6, 13.6, 14.6.1, 15.6.1, 26.6.2 and
  27.0.1 boot under HVF, and of 13.6 to 27.0.1 under TCG, with SIP enabled
  and no boot arguments.
- Reads the guest's disks through the machine's firmware disk interface,
  with read-only GPT and APFS readers, and unpacks the boot objects from
  their Image4 containers.
- Writes the device tree from the guest's own template and the kernel's
  boot arguments as iBoot does, for the devices the machine has.
- Sets the pointer authentication keys for the kernel and every process,
  and turns into `nop`s the loops where macOS 14 and later spin when the
  hypervisor refuses their pointer authentication calls.
- Adds kexts named in a configuration bundle in an Auxiliary kernel
  collection it builds in memory, which the kernel starts with SIP on, and
  records where each went under `/chosen/ambrosia` for injected code.
- A configuration bundle, loaded by QEMU at `0x120000000`, whose
  `config.plist` sets boot arguments, adds kexts, patches kernel collection
  entries, exports vtable discriminators and names a graphics driver
  module.
- `ambrosia-bundle`, which builds and checks configuration bundles.
- The graphics driver module interface, revision 1, with an example module
  for QEMU's `bochs-display` that shows the boot's log as an early console.
- A log of each step on the machine's UART, and an error line that names
  the problem when the boot can't go on.
- `scripts/run-guest.sh`, which boots a guest from a VM directory and an
  ECID.
- A reproducible build: a commit builds the same image on any host, and
  `make verify-release` checks a release against its commit.
