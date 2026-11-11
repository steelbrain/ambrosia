# Changelog

Every release of Ambrosia, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each
release's image is published with the release.

## Unreleased

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
