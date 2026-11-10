# Ambrosia's build. `make` builds the image, target/ambrosia.bin; `make help`
# lists every target.

TARGET := aarch64-unknown-none
SYSROOT := $(shell rustc --print sysroot)
HOST := $(shell rustc -vV | sed -n 's/^host: //p')
RUSTC_COMMIT := $(shell rustc -vV | sed -n 's/^commit-hash: //p')
OBJCOPY := $(SYSROOT)/lib/rustlib/$(HOST)/bin/llvm-objcopy
CARGO_HOME ?= $(HOME)/.cargo

# Extra flags for every cargo command. CI passes --locked.
CARGO_FLAGS ?=

# Where `make release` writes a release, and where `make verify-release` reads
# one.
DIST ?= dist

# sha256sum on Linux, shasum on macOS: both write and check the same format.
SHA256 := $(shell command -v sha256sum >/dev/null 2>&1 && echo sha256sum || echo shasum -a 256)

# The compiler writes source paths into the image, in panic locations. These
# map the machine-specific ones to fixed names: the checkout, crates.io's
# sources, and the toolchain's library sources, to the /rustc/<commit> form
# they take on a machine without the rust-src component. With them the same
# commit builds the same image on every machine (`make verify-release`).
REMAP := \
	--remap-path-prefix=$(CURDIR)=/ambrosia \
	--remap-path-prefix=$(CARGO_HOME)/registry/src=/cargo/registry/src \
	--remap-path-prefix=$(SYSROOT)/lib/rustlib/src/rust=/rustc/$(RUSTC_COMMIT)
# Given with --config, the flags join any rustflags a .cargo/config.toml sets
# for the target, such as the driver module's.
RUSTFLAGS_CONFIG := --config 'target.$(TARGET).rustflags=[$(foreach f,$(REMAP),"$(f)",)]'

ELF := target/$(TARGET)/release/ambrosia

# The example graphics driver module is a crate and workspace of its own,
# built from its directory so that its .cargo/config.toml applies.
DRIVER := drivers/bochs-display
DRIVER_TARGET := $(CURDIR)/target/drivers

.DEFAULT_GOAL := image
.PHONY: help image test clippy fmt lint ci bochs-display bochs-display-bundle \
	hello-kext hello-kext-bundle release verify-release clean

help: ## List the targets.
	@awk -F ':.*## ' '/^[a-z-]+:.*## / { printf "  %-22s %s\n", $$1, $$2 }' \
		$(MAKEFILE_LIST)

# The image builds from its crate's directory, so that its .cargo/config.toml
# applies: the target and the linker script.
image: ## Build the firmware image, target/ambrosia.bin.
	cd crates/firmware && cargo build $(CARGO_FLAGS) --release $(RUSTFLAGS_CONFIG)
	$(OBJCOPY) -O binary $(ELF) target/ambrosia.bin
	@ls -l target/ambrosia.bin

test: bochs-display ## Run the host tests, the example module's load among them.
	AMBROSIA_TEST_DRIVER=$(CURDIR)/target/bochs-display.elf \
		cargo test $(CARGO_FLAGS) -p ambrosia -p ambrosia-bundle

clippy: ## Lint every crate; warnings fail.
	cargo clippy $(CARGO_FLAGS) -p ambrosia -p ambrosia-bundle --all-targets -- -D warnings
	cd crates/firmware && cargo clippy $(CARGO_FLAGS) -- -D warnings
	cd $(DRIVER) && cargo clippy $(CARGO_FLAGS) --target-dir $(DRIVER_TARGET) -- -D warnings

fmt: ## Format every crate.
	cargo fmt --all
	cd $(DRIVER) && cargo fmt

lint: ## Check formatting, then lint.
	cargo fmt --all --check
	cd $(DRIVER) && cargo fmt --check
	$(MAKE) clippy

ci: lint test bochs-display-bundle release verify-release ## Run everything CI runs; release needs a clean checkout.

bochs-display: ## Build the example driver module, target/bochs-display.elf.
	cd $(DRIVER) && cargo build $(CARGO_FLAGS) --release \
		--target-dir $(DRIVER_TARGET) $(RUSTFLAGS_CONFIG)
	# The linker names its own build in .comment, which differs between the
	# toolchain's macOS and Linux builds; nothing reads it, so it goes, and
	# the module is the same bytes on every host, as the image is.
	$(OBJCOPY) --remove-section=.comment \
		$(DRIVER_TARGET)/$(TARGET)/release/bochs-display target/bochs-display.elf
	@ls -l target/bochs-display.elf

# ambrosia-bundle puts the module at Drivers/bochs-display.elf, where the
# configuration names it.
bochs-display-bundle: bochs-display ## Build a bundle that starts it, target/bochs-display.cpio.
	cargo run $(CARGO_FLAGS) -q -p ambrosia-bundle -- create \
		--config $(DRIVER)/config.plist --driver target/bochs-display.elf \
		--output target/bochs-display.cpio

# The example kext builds with the macOS SDK, so these two run only on macOS,
# and ci leaves them out.
hello-kext: ## Build the example kext, target/hello-kext/Hello.kext (macOS).
	examples/hello-kext/build.sh target/hello-kext

hello-kext-bundle: hello-kext ## Build a bundle that adds it, target/hello-kext.cpio (macOS).
	cargo run $(CARGO_FLAGS) -q -p ambrosia-bundle -- create \
		--config examples/hello-kext/config.plist \
		--kext target/hello-kext/Hello.kext --output target/hello-kext.cpio

# A release is what gets published for a commit: the image, its SHA-256, the
# license notices of the crates it links in, and the commit it was built
# from. Untracked files can change a build too (a build.rs, a
# .cargo/config.toml), so the checkout must have none.
release: ## Build a release from a clean checkout into dist/ (or DIST).
	@if [ -n "$$(git status --porcelain)" ]; then \
		echo "release: commit or remove these changes first:" >&2; \
		git status --short >&2; exit 1; \
	fi
	$(MAKE) image
	mkdir -p "$(DIST)"
	cd "$(DIST)" && rm -f ambrosia.bin SHA256SUMS THIRD-PARTY.md COMMIT
	install -m 644 target/ambrosia.bin "$(DIST)/ambrosia.bin"
	cd "$(DIST)" && $(SHA256) ambrosia.bin > SHA256SUMS
	scripts/internal/third-party.sh > "$(DIST)/THIRD-PARTY.md"
	git rev-parse HEAD > "$(DIST)/COMMIT"
	@cat "$(DIST)/SHA256SUMS"

# Checks a release, built here or anywhere else, against its commit: the
# checksum against the image, then a rebuild of the commit, in a temporary
# worktree and so from another directory, against the image and the notices.
verify-release: ## Rebuild the release in dist/ (or DIST) from its commit and compare.
	@set -e; \
	[ -d "$(DIST)" ] || { echo "verify-release: no release in $(DIST); make release writes one" >&2; exit 1; }; \
	dist=$$(cd "$(DIST)" && pwd); \
	for f in ambrosia.bin SHA256SUMS THIRD-PARTY.md COMMIT; do \
		[ -f "$$dist/$$f" ] || { echo "verify-release: no $(DIST)/$$f" >&2; exit 1; }; \
	done; \
	commit=$$(cat "$$dist/COMMIT"); \
	(cd "$$dist" && $(SHA256) -c SHA256SUMS); \
	dir=$$(mktemp -d); \
	trap 'git worktree remove --force "$$dir" >/dev/null 2>&1 || true; rm -rf "$$dir"' EXIT; \
	echo "verify-release: rebuilding $$commit in $$dir"; \
	git worktree add --quiet --detach "$$dir" "$$commit"; \
	$(MAKE) -C "$$dir" image CARGO_FLAGS="$(CARGO_FLAGS)" >/dev/null; \
	cmp -s "$$dir/target/ambrosia.bin" "$$dist/ambrosia.bin" || \
		{ echo "verify-release: $$commit builds a different image from $(DIST)/ambrosia.bin" >&2; exit 1; }; \
	(cd "$$dir" && scripts/internal/third-party.sh) | cmp -s - "$$dist/THIRD-PARTY.md" || \
		{ echo "verify-release: $$commit gives different notices from $(DIST)/THIRD-PARTY.md" >&2; exit 1; }; \
	echo "verify-release: $(DIST) is the release of $$commit"

clean: ## Remove build output, dist/ among it.
	cargo clean
	rm -rf $(DRIVER_TARGET) dist
