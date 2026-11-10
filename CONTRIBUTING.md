# Contributing to Ambrosia

Thanks for helping. This page says how to build and check a change, how to
write its commits, the one rule about Apple's code, and how to report a
bug.

## Building and testing

You need Rust through rustup, which installs the toolchain
`rust-toolchain.toml` pins on the first build, and `make`.
[Building](docs/building.md) has the details.

```sh
make            # the image, target/ambrosia.bin
make test       # host tests, including loading the example driver module
make lint       # rustfmt, then clippy; any warning fails
make fmt        # format every crate
make ci         # everything CI runs
```

`make ci` ends by making a release and verifying it, so it needs a clean
checkout: commit your change first, or run `make lint test` while you work.

Most of the code is in `crates/ambrosia`, which is `no_std` but builds and
tests on the host. Some of its tests read a real guest's disk, and are
skipped unless their environment variables are set;
[crates/ambrosia](crates/ambrosia/README.md#building-and-testing) lists
them. A change to how the boot reads or writes something should come with
a test that runs without a guest, as most of the existing ones do, built
from bytes the test writes itself.

Boot a guest with your change before you send it, if it touches the boot
([running a guest](docs/running.md)), and say in the pull request which
macOS builds and which accelerator you booted.

## Linting and style

- `make lint` must pass. The workspace's lints are in `Cargo.toml`; among
  them, every `unsafe` block needs a `// SAFETY:` comment that says why it
  is sound.
- Keep it simple. Handle what the machine and macOS give, and add no
  option where the firmware can decide from what it finds.
- The core names no product. Which kexts are injected, which patches are
  applied and which device a driver module drives come from the
  configuration, not from the code.
- There is no code for Apple's paravirtual devices (the apple-gfx display,
  the IOSurface mapper, the paravirtual video devices), and none should be
  added.

## Reproducible builds

A commit builds the same image, byte for byte, on any host. The Makefile
maps machine-specific paths to fixed names, and the toolchain is pinned.
Don't add anything to the build that depends on the machine, the time or
the checkout's path. CI builds the release on Linux and on macOS and fails
if the two differ, and `make verify-release` rebuilds a release from its
commit and compares.

Both lockfiles, `Cargo.lock` and `drivers/bochs-display/Cargo.lock`, are
committed, and CI builds with `--locked`. Commit lockfile changes only when
you change a dependency.

## Documentation

A change that alters what a user sees updates the docs in the same change:
a configuration key, a boot step, a log line, the driver module interface,
what the machine must give, or the build. That means `docs/`, the README
where it says the same thing, and a line under an `## Unreleased` heading
at the top of `CHANGELOG.md`, which you add if it isn't there.

Docs say what is true now, in plain sentences, without dates or the
history of how something came to be. Internals pages say why as well as
what, and name the code that acts on each fact.

## Commits

- One logical change per commit, with the docs and tests it needs.
- A subject of at most 72 characters, in the imperative mood, after an
  emoji code for its kind: `:new:` for a feature, `:bug:` for a fix,
  `:art:` for a cleanup that changes no behavior, `:fire:` for a removal,
  `:memo:` for documentation, `:wrench:` for the build or CI, `:truck:` for
  a move or rename.
- A body, wrapped at 80 columns, that says why the change is needed.
- Rebase on the main branch rather than merging it into yours.

```text
:bug: Resolve a kext's binds against every collection member

A kext imports KPIs the kernel re-exports from other members of the
collection, such as com.apple.kpi.iokit. Resolving against the kernel's
own symbols alone failed those binds and stopped the boot.
```

## Apple's code

No Apple binaries or code go in this repository: not AVPBooter, iBoot, a
kernel collection, a device tree template, or anything taken from them.
That includes test fixtures, guest dumps, and logs that carry Apple data
such as a kernel's memory.

Disassembling Apple's binaries to learn what they do is fine, and much of
what Ambrosia does was learned that way. Copying from them is not: write
the code from your own understanding, and describe what you learned in
your own words.

## Reporting a bug

Open an issue, and include:

- the Ambrosia version or commit, and whether you built it or took a
  release image;
- the QEMU you ran, and how it was built;
- the host, and the accelerator: HVF or TCG;
- the guest's macOS build, such as 15.6.1 (24G90);
- the command line, or the `scripts/run-guest.sh` options;
- the `config.plist`, if you used a bundle;
- the whole serial log. If the boot got past Ambrosia, boot again with
  `serial=3 -v` boot arguments and include the kernel's log too
  ([reading the boot log](docs/how-to/troubleshoot-a-boot.md)).

Replace the guest's ECID, serial number and volume UUIDs in the log, as the
docs' examples do. Don't attach disk images, kernel collections or other
Apple files.
