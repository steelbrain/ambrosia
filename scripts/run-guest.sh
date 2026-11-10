#!/bin/sh
# Boots an arm64 macOS guest on QEMU's vmapple machine with Ambrosia as its
# firmware. Run `scripts/run-guest.sh -h` for the options; docs/running.md
# explains each part of the command line it builds.
set -eu

usage() {
    cat <<'EOF'
usage: scripts/run-guest.sh -e ECID [options] VM_DIR [-- QEMU_ARGS...]

VM_DIR holds the guest's disk.img and aux.img. QEMU takes aux.img without its
first 16 KiB block; the first run writes that copy as aux.img.trimmed, and the
guest keeps its NVRAM there from then on.

  -e ECID      the machine's ECID, in decimal or 0x hex (required)
  -f FILE      the firmware image (default: target/ambrosia.bin, from make)
  -b FILE      a configuration bundle to load at 0x120000000
  -q FILE      the QEMU to run (default: $QEMU, else qemu-system-aarch64)
  -t           run under TCG instead of HVF
  -m SIZE      memory (default: 8G)
  -c COUNT     CPUs (default: 4)
  -p PORT      forward 127.0.0.1:PORT to the guest's SSH port (default: 10022)
  -s FILE      write the serial log to FILE instead of the terminal
  -h           print this

Arguments after -- go to QEMU as they are.
EOF
}

root=$(cd "$(dirname "$0")/.." && pwd)
ecid='' firmware=$root/target/ambrosia.bin bundle='' qemu=${QEMU:-qemu-system-aarch64}
accel=hvf memory=8G cpus=4 port=10022 serial=mon:stdio

while getopts e:f:b:q:tm:c:p:s:h opt; do
    case $opt in
        e) ecid=$OPTARG ;;
        f) firmware=$OPTARG ;;
        b) bundle=$OPTARG ;;
        q) qemu=$OPTARG ;;
        t) accel=tcg ;;
        m) memory=$OPTARG ;;
        c) cpus=$OPTARG ;;
        p) port=$OPTARG ;;
        s) serial=file:$OPTARG ;;
        h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
shift $((OPTIND - 1))

if [ -z "$ecid" ] || [ $# -lt 1 ]; then
    usage >&2
    exit 2
fi
vm=$1
shift
[ "${1:-}" = "--" ] && shift

if [ ! -f "$firmware" ]; then
    echo "run-guest.sh: no such file: $firmware (run make, or pass -f)" >&2
    exit 1
fi
[ -f "$vm/disk.img" ] || { echo "run-guest.sh: no such file: $vm/disk.img" >&2; exit 1; }
if [ ! -f "$vm/aux.img.trimmed" ]; then
    [ -f "$vm/aux.img" ] || { echo "run-guest.sh: no such file: $vm/aux.img" >&2; exit 1; }
    echo "run-guest.sh: writing $vm/aux.img.trimmed" >&2
    dd if="$vm/aux.img" of="$vm/aux.img.trimmed" bs=16384 skip=1 2>/dev/null
fi

if [ "$accel" = hvf ]; then
    accel_args="-accel hvf,kernel-irqchip=on"
else
    accel_args="-accel tcg,thread=multi"
fi
if [ -n "$bundle" ]; then
    [ -f "$bundle" ] || { echo "run-guest.sh: no such file: $bundle" >&2; exit 1; }
    set -- -device "loader,file=$bundle,addr=0x120000000,force-raw=on" "$@"
fi

# shellcheck disable=SC2086 # accel_args is two words on purpose
exec "$qemu" $accel_args \
    -M "vmapple,uuid=$ecid" -m "$memory" -smp "$cpus" \
    -bios "$firmware" \
    -drive "file=$vm/aux.img.trimmed,if=pflash,format=raw" \
    -drive "file=$vm/disk.img,if=pflash,format=raw" \
    -drive "file=$vm/aux.img.trimmed,if=none,id=aux,format=raw" \
    -drive "file=$vm/disk.img,if=none,id=root,format=raw" \
    -device vmapple-virtio-blk-pci,variant=aux,drive=aux \
    -device vmapple-virtio-blk-pci,variant=root,drive=root \
    -netdev "user,id=net0,ipv6=off,hostfwd=tcp:127.0.0.1:$port-:22" \
    -device virtio-net-pci,netdev=net0,romfile= \
    -display none -serial "$serial" \
    "$@"
