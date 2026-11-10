#!/bin/sh
# Prints, as Markdown, the crates.io crates linked into the firmware image,
# each with its license and its license text: the notice a release carries as
# THIRD-PARTY.md beside its image. `make release` runs it, from the
# repository's root after the image's build, so that cargo has the crates'
# sources.
set -eu

target=aarch64-unknown-none
registry=${CARGO_HOME:-$HOME/.cargo}/registry/src

cat <<'HEAD'
# Third-party crates in ambrosia.bin

The image links in these crates from crates.io. Each is used under the
license given here; where a crate offers a choice, the MIT license is the
one taken. Each license's text, with the crate's copyright notice, follows.
HEAD

cargo tree -q -p ambrosia-firmware --target "$target" -e normal --prefix none \
    --format '{p}|{l}' |
    grep -v -e '^ambrosia ' -e '^ambrosia-firmware ' |
    sed 's/ (\*)$//' | sort -u |
    while IFS='|' read -r package license; do
        name=${package% v*}
        version=${package##* v}
        dir=$(find "$registry" -maxdepth 2 -type d -name "$name-$version" | head -n 1)
        text=
        for f in LICENSE-MIT LICENSE-MIT.md LICENSE LICENSE.md LICENSE.txt; do
            if [ -f "$dir/$f" ]; then text=$dir/$f; break; fi
        done
        if [ -z "$text" ]; then
            echo "third-party.sh: no license text for $name $version" >&2
            exit 1
        fi
        printf '\n## %s %s\n\nLicense: %s\n\n```text\n' "$name" "$version" "$license"
        cat "$text"
        printf '```\n'
    done
