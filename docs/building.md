# Building

The Makefile at the repository's root drives Cargo. `make` builds the
firmware image, `target/ambrosia.bin`, the one file QEMU needs from this
repository. `make help` lists every target.

## Requirements

- Rust, installed with rustup. `rust-toolchain.toml` pins the toolchain to
  1.99.1, with the `aarch64-unknown-none` target and the `clippy`, `rustfmt`
  and `llvm-tools` components, and rustup installs them on the first build.
  `llvm-tools` provides the `llvm-objcopy` that turns the linked ELF into
  the flat image.
- `make`.

The image is built for a bare-metal arm64 target, so any host Rust supports
can build it. CI builds it on x86-64 Linux.

## Targets

- `make`, or `make image`, builds the firmware image,
  `target/ambrosia.bin`.
- `make test` builds the example driver module, then runs the host tests.
- `make clippy` lints every crate. Any warning fails.
- `make fmt` formats every crate.
- `make lint` checks formatting, then runs `make clippy`.
- `make bochs-display` builds the example graphics driver module,
  `target/bochs-display.elf`.
- `make bochs-display-bundle` builds a configuration bundle that starts it,
  `target/bochs-display.cpio`.
- `make release` builds a release from a clean checkout into `dist/`.
- `make verify-release` rebuilds the release in `dist/` from its commit and
  compares the bytes.
- `make ci` runs `lint`, `test`, `bochs-display-bundle`, `release` and
  `verify-release`, as CI does. Since it makes a release, it needs a clean
  checkout.
- `make clean` removes the build output, `dist/` among it.

`CARGO_FLAGS` is added to every Cargo command. CI sets it to `--locked`.

A bare `cargo build` or `cargo test` at the root builds and tests the core
library, `crates/ambrosia`, on the host. The firmware builds only for its
bare-metal target, through `make image`.

### image

Builds the `ambrosia-firmware` crate in release mode from its directory,
so that its `.cargo/config.toml` applies: it targets `aarch64-unknown-none`
and links with `crates/firmware/link.ld`, which places the image at
0x00100000, where the `vmapple` machine maps its firmware. `llvm-objcopy`
then writes the loadable bytes as `target/ambrosia.bin`. The image is under
200 KiB, and its code, data, bss and 64 KiB stack all fit in the machine's
1 MiB firmware region.

The ELF stays at `target/aarch64-unknown-none/release/ambrosia`, with debug
information, for reading addresses from an exception line in the boot log
(see [crates/firmware](../crates/firmware/README.md#reading-an-exception)).

### test

Runs the host tests of the core library, `crates/ambrosia`. The target
first builds the example module and passes its absolute path in
`AMBROSIA_TEST_DRIVER`, so the loader test loads a real module. The tests
that read a real guest's disk are skipped unless their variables are set;
[crates/ambrosia](../crates/ambrosia/README.md#building-and-testing) lists
them.

### bochs-display and bochs-display-bundle

`make bochs-display` builds `drivers/bochs-display`, the example graphics
driver module, as `target/bochs-display.elf`. The module is a crate and
workspace of its own, built from its directory so that its
`.cargo/config.toml` links it as a static PIE.

`make bochs-display-bundle` puts that module and
`drivers/bochs-display/config.plist`, which names it, in a configuration
bundle, `target/bochs-display.cpio`:

```text
config.plist
Drivers/bochs-display.elf
```

[Running](running.md#the-configuration-bundle) shows how QEMU loads it,
and [graphics drivers](graphics-drivers.md#the-example-bochs-display) what
it does.

## The bundle tool

`ambrosia-bundle`, in `crates/ambrosia-bundle`, assembles and checks
configuration bundles. It runs on the host:

```sh
cargo build --release -p ambrosia-bundle   # target/release/ambrosia-bundle
```

[Building a bundle](how-to/build-a-bundle.md) shows how to use it.

## Reproducible builds

The compiler writes source paths into the image, in panic messages. The
build maps the paths that differ between machines, those of the checkout,
of Cargo's registry and of the toolchain's sources, to fixed names, and
`rust-toolchain.toml` pins the toolchain. So a commit builds the same
`ambrosia.bin`, byte for byte, on any host. CI checks this on every push:
it builds the release on Linux and on macOS and compares the two.

## Releases

`make release` builds the image and writes a release into `dist/`:

- `ambrosia.bin`, the image;
- `SHA256SUMS`, the image's SHA-256;
- `THIRD-PARTY.md`, the license notices of the crates.io crates linked
  into the image, which `scripts/internal/third-party.sh` writes;
- `COMMIT`, the commit it was built from.

It refuses to run while the checkout has changes or untracked files, since
either could change the build. `DIST=dir` writes the release somewhere
else. The files are published with the release; `dist/` is not committed.

`make verify-release` checks a release, built here or anywhere else:
`SHA256SUMS` against the image, then the image and the notices against a
fresh build of `COMMIT`, made in a temporary worktree. `DIST=dir` checks a
release in another directory, such as one downloaded from a release page.
It says which file differs when one does.

CI (`.github/workflows/ci.yml`) runs `make ci` on Linux and on macOS, with
`--locked`.
