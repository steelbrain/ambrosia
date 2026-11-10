#!/bin/sh
# Builds the zip that a release page attaches, dist/ambrosia-<version>.zip,
# from the clean checkout it runs in: `make release`, then
# `make verify-release`, then the release's files and Ambrosia's license
# texts in one folder, ambrosia-<version>/. <version> is Cargo.toml's.
# DIST=dir writes the release and the zip there instead of dist/. CI runs it
# on every push, on Linux and on macOS, and compares the two zips; for a
# pushed tag, CI then publishes the zip as the tag's release.
#
# The zip is the same bytes on every host, as the image is. Its files are
# stored uncompressed, in a fixed order, each with mode 644 and the time of
# the release's commit, so rebuilding the zip from the tag checks it as
# `make verify-release` checks the image. It needs python3, for its zipfile
# module.
set -eu
export LC_ALL=C

cd "$(git rev-parse --show-toplevel)"
dist=${DIST:-dist}
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
if [ -z "$version" ]; then
    echo "release-zip.sh: Cargo.toml has no [workspace.package] version" >&2
    exit 1
fi

make release DIST="$dist"
make verify-release DIST="$dist"

commit=$(cat "$dist/COMMIT")
top=ambrosia-$version
# The zip's members, each as its source and its name in the folder.
set -- \
    "$dist/ambrosia.bin" ambrosia.bin \
    "$dist/SHA256SUMS" SHA256SUMS \
    "$dist/THIRD-PARTY.md" THIRD-PARTY.md \
    "$dist/COMMIT" COMMIT \
    LICENSE LICENSE
for f in LICENSES/*; do
    set -- "$@" "$f" "$f"
done

python3 -I - "$dist/$top.zip" "$top" "$(git log -1 --format=%ct "$commit")" "$@" <<'EOF'
import os
import sys
import time
import zipfile

out, top, when = sys.argv[1:4]
members = sys.argv[4:]
date = time.gmtime(int(when))[:6]
tmp = out + ".tmp"
with zipfile.ZipFile(tmp, "w", zipfile.ZIP_STORED) as z:
    for src, name in zip(members[0::2], members[1::2]):
        info = zipfile.ZipInfo(top + "/" + name, date_time=date)
        info.create_system = 3  # Unix, so that the mode below applies.
        info.external_attr = 0o100644 << 16
        with open(src, "rb") as f:
            z.writestr(info, f.read())
os.replace(tmp, out)
EOF

if command -v sha256sum > /dev/null 2>&1; then
    (cd "$dist" && sha256sum "$top.zip")
else
    (cd "$dist" && shasum -a 256 "$top.zip")
fi
