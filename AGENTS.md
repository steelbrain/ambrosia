# Ambrosia: a guide for coding agents

This file is for coding agents working in this repository. People should
read [README.md](README.md) and [CONTRIBUTING.md](CONTRIBUTING.md) first;
everything here agrees with them.

## What the project is

Ambrosia is boot firmware for arm64 macOS guests on QEMU's `vmapple`
machine. QEMU loads it with `-bios` in place of Apple's AVPBooter. It runs
from the machine's 1 MiB firmware region at 0x00100000, reads macOS's boot
objects from the guest's disk, lays out the kernel collection, device tree
and boot arguments as Apple's iBoot would, and enters the kernel with SIP
on. Apple's firmware never runs. A configuration bundle can add kexts,
which Ambrosia places in an Auxiliary kernel collection it builds in
memory, so the kernel starts them with SIP still on.

It is general purpose. The core names no product, kext or device: which
kexts are injected, which patches are applied and which graphics driver
module runs all come from the configuration.

[docs/README.md](docs/README.md) indexes the documentation.
[docs/boot-process.md](docs/boot-process.md) is the boot step by step, and
[docs/internals/](docs/internals/) explains why each step is the way it is.
Read [docs/internals/lessons-learned.md](docs/internals/lessons-learned.md)
before changing how the kernel collection, the Auxiliary KC, the device
tree or the memory layout is built: each item there is a boot that hung.

## Layout

| Path | What it holds |
|---|---|
| `crates/ambrosia` | The core library, `no_std` with `alloc`: GPT, APFS, Image4, the kernel collection, the Auxiliary KC, the device tree, the boot arguments, the configuration. Builds and tests on the host. |
| `crates/firmware` | The image: `start.s`, the MMU, the UART, the firmware disk interface, the driver module loader's machine side, and `main.rs`, the boot's sequence. Builds only for `aarch64-unknown-none`. |
| `crates/ambrosia-bundle` | The host tool that builds and checks configuration bundles with the core's parsers. |
| `drivers/bochs-display` | The example graphics driver module, a workspace of its own. |
| `examples/hello-kext` | A minimal kext to inject. |
| `scripts/run-guest.sh` | Boots a guest. |
| `scripts/internal` | Maintainer tools; `third-party.sh` writes a release's license notices. |
| `docs` | User docs at the top, task guides in `how-to/`, design in `internals/`. |

Everything that knows the machine, its addresses and its config block, is
in `crates/firmware`. The core takes the machine as values (`tree::Machine`)
and reads disks through the `BlockDevice` trait, which is what lets it run
on the host in tests.

## Commands

```sh
make            # the image, target/ambrosia.bin
make test       # host tests; builds the example driver module first
make lint       # cargo fmt --check, then clippy with -D warnings
make fmt        # format every crate
make ci         # lint, test, bochs-display-bundle, release, verify-release
make help       # every target
```

- `make lint` and `make test` must pass before a change is done.
- `make ci` makes a release, which refuses a checkout with changes or
  untracked files. Commit first, or run `make lint test`.
- A bare `cargo test` at the root runs the core's tests on the host. The
  firmware builds only through `make image`, from its own directory, so
  that its `.cargo/config.toml` applies.
- The example driver module is its own workspace; the Makefile builds and
  lints it from `drivers/bochs-display`.
- Tests that read a real guest's disk are skipped unless
  `AMBROSIA_TEST_DISK`, `AMBROSIA_TEST_AUX` or `AMBROSIA_TEST_BOOT_FILES`
  is set ([crates/ambrosia](crates/ambrosia/README.md#building-and-testing)).
  Don't depend on them for a change's tests; build the input bytes in the
  test, as the existing tests do.
- Booting a guest needs a QEMU with the `vmapple` changes
  [docs/machine.md](docs/machine.md) lists and an installed guest. If you
  can't boot one, say so rather than claim the change works.

## Rules

**Provenance.** No Apple binaries or code in the tree: not AVPBooter,
iBoot, kernel collections, device tree templates, or bytes, tables or code
taken from them. That covers test fixtures, guest dumps and logs that
carry Apple data. Disassembling Apple's binaries to learn what they do is
fine; copying from them is not.

**Docs follow behavior.** A change a user can see updates, in the same
change, the docs that describe it: `docs/`, the README where it says the
same, and a line under `## Unreleased` in `CHANGELOG.md`. That includes a
new or changed configuration key, log line, error, boot step, driver
module interface field, machine requirement or build target. Docs state
what is true now, in plain sentences, with no dates or history. Check each
claim against the code before you write it.

**Keep it general and simple.**
- No code, configuration key or handover field for Apple's paravirtual
  devices: the apple-gfx display, the IOSurface mapper, the paravirtual
  video devices.
- No product names, IDs or special cases in the core.
- Handle what the machine and macOS give. Add no option where the
  firmware can decide from what it finds.
- Every error stops the boot with a line that names the problem. Parsers of
  disk, bundle and device tree data check sizes and offsets and return an
  error rather than panic or compute a wrong address.

**Reproducible builds.** A commit builds the same image on every host.
Don't add anything to the build that depends on the machine, the time or
the checkout's path. Both lockfiles are committed and CI builds with
`--locked`.

**Rust style.**
- Edition 2024. The workspace lints in `Cargo.toml` apply; among them,
  every `unsafe` block needs a `// SAFETY:` comment, and `unsafe fn`s need
  a `# Safety` section.
- The core is `no_std` with `alloc`, and must stay so. Its only
  dependencies are `compcol` and `font8x8`. Add one only when the change
  can't do without it, and say why in the commit.
- Doc comments say what a thing is and why, in full sentences.

**Commits.** One logical change per commit, with its tests and docs. A
subject of at most 72 characters, imperative, after an emoji code
(`:new:`, `:bug:`, `:art:`, `:fire:`, `:memo:`, `:wrench:`, `:truck:`), and
a body wrapped at 80 columns that says why. Stage explicit paths. Don't
rewrite history others have pulled; fix forward with a new commit.
