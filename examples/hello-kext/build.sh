#!/bin/sh
# Builds Hello.kext, the example kext, in the form Ambrosia places: an arm64e
# MH_KEXT_BUNDLE with chained fixups, whose imports from the kernel are
# flat-namespace binds. It needs the macOS SDK and nothing else: no Kernel
# Debug Kit, no Xcode project.
#
# Usage: build.sh [OUT_DIR]
# Writes OUT_DIR/Hello.kext. OUT_DIR defaults to build/ beside this script.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/build}
kext=$out/Hello.kext
sdk=$(xcrun --show-sdk-path)
obj=$(mktemp -d)
trap 'rm -rf "$obj"' EXIT

# Kernel code: no C++ exceptions, RTTI or standard library, and the kernel's
# own headers in place of the user-space ones.
xcrun clang -target arm64e-apple-macos11 -x c++ -std=c++17 -Wall \
  -mkernel -fapple-kext -nostdinc \
  -fno-builtin -fno-common -fno-exceptions -fno-rtti -fno-stack-protector \
  -I"$sdk/System/Library/Frameworks/Kernel.framework/Headers" \
  -c "$here/Hello.cpp" -o "$obj/Hello.o"

# -kext makes an MH_KEXT_BUNDLE. -undefined dynamic_lookup leaves the
# kernel's symbols as flat-namespace imports, which Ambrosia binds at boot
# against the kernel collection on the guest's disk. -fixup_chains writes
# the pointers as chained fixups, the form Ambrosia reads.
rm -rf "$kext"
mkdir -p "$kext/Contents/MacOS"
xcrun clang -target arm64e-apple-macos11 -nostdlib -fapple-kext \
  -Xlinker -kext -Xlinker -undefined -Xlinker dynamic_lookup \
  -Xlinker -fixup_chains \
  "$obj/Hello.o" -o "$kext/Contents/MacOS/Hello"
cp "$here/Info.plist" "$kext/Contents/Info.plist"
echo "built $kext"
