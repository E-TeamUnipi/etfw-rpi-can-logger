#!/bin/sh
# Build the SD card image with Buildroot, natively on the build machine.
#
#   scripts/build-image.sh [--docker|--native] [--clean] [-j N] rpi3|rpi4|rpi5 [make targets]
#
#   --native  build directly (default on Linux, x86_64 or aarch64)
#   --docker  build in a container for the host's own architecture
#             (default on macOS; arm64 on Apple Silicon, no emulation)
#   --clean   start this board's output from scratch (needed after changing
#             the toolchain or CPU in a defconfig)
#   -j N      parallel jobs (default: number of CPUs)
#
# Downloads, the compiler cache (ccache) and Rust crates are kept between
# builds, so another board or a rebuild only compiles what changed. The Rust
# workspace is re-synced from this checkout every time, so edits are picked up.
# Result: output-<board>/images/sdcard.img
set -e

BR_VERSION=2026.08
HERE=$(cd "$(dirname "$0")/.." && pwd)

MODE=
CLEAN=
JOBS=
while [ $# -gt 0 ]; do
	case $1 in
	--docker) MODE=docker ;;
	--native) MODE=native ;;
	--clean) CLEAN=1 ;;
	-j) JOBS=$2; shift ;;
	-j*) JOBS=${1#-j} ;;
	-h|--help) sed -n '2,16p' "$0"; exit 0 ;;
	-*) echo "unknown option: $1" >&2; exit 2 ;;
	*) break ;;
	esac
	shift
done
BOARD=${1:-rpi4}
[ $# -gt 0 ] && shift
DEFCONFIG=canlogger_${BOARD}_defconfig
if [ ! -f "$HERE/buildroot/configs/$DEFCONFIG" ]; then
	echo "unknown board '$BOARD' (rpi3, rpi4, rpi5)" >&2
	exit 2
fi
if [ -z "$MODE" ]; then
	if [ "$(uname -s)" = Linux ]; then MODE=native; else MODE=docker; fi
fi

# network hiccups: Buildroot resumes where it stopped
retry() {
	for i in 1 2 3; do
		"$@" && return 0
		[ $i = 3 ] && return 1
		echo "*** failed, retrying in 30 s ($i/3): $*" >&2
		sleep 30
	done
}

if [ $MODE = docker ]; then
	IMAGE=canlogger-builder
	VOL=${CANLOGGER_VOLUME:-canlogger-br}
	docker build -q -t $IMAGE "$HERE/scripts" >/dev/null
	docker volume create $VOL >/dev/null
	docker run --rm -u 0 -v $VOL:/br $IMAGE chown 1000:1000 /br

	# copy the checkout in (no bind mount: Docker Desktop may not share its folder)
	TAR_OPTS=
	if [ "$(uname -s)" = Darwin ]; then
		TAR_OPTS="--no-xattrs --no-mac-metadata"
		export COPYFILE_DISABLE=1
	fi
	tar -C "$HERE" $TAR_OPTS --exclude ./target --exclude './output-*' \
		--exclude ./buildroot-src --exclude ./.git --exclude .DS_Store -cf - . |
		docker run --rm -i -v $VOL:/br $IMAGE sh -c 'rm -rf /br/src && mkdir /br/src && tar -C /br/src -xf -'

	TTY=
	[ -t 1 ] && TTY=-t
	docker run --rm -i $TTY -v $VOL:/br \
		-e CANLOGGER_CACHE=/br/cache -e CANLOGGER_OUT=/br/output-$BOARD -e CANLOGGER_IN_DOCKER=1 \
		$IMAGE sh /br/src/scripts/build-image.sh --native ${CLEAN:+--clean} ${JOBS:+-j $JOBS} $BOARD "$@"

	if [ $# -eq 0 ]; then
		mkdir -p "$HERE/output-$BOARD/images"
		docker run --rm -v $VOL:/br $IMAGE cat /br/output-$BOARD/images/sdcard.img \
			> "$HERE/output-$BOARD/images/sdcard.img"
		echo "Image: $HERE/output-$BOARD/images/sdcard.img"
	fi
	exit 0
fi

if [ "$(uname -s)" != Linux ]; then
	echo "native builds need Linux; use --docker" >&2
	exit 2
fi

CACHE=${CANLOGGER_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/canlogger}
OUT=${CANLOGGER_OUT:-$HERE/output-$BOARD}
SRC=$CACHE/buildroot-$BR_VERSION
JOBS=${JOBS:-$(nproc)}
export BR2_DL_DIR=$CACHE/dl BR2_CCACHE_DIR=$CACHE/ccache
mkdir -p "$CACHE"

if [ ! -d "$SRC" ]; then
	rm -rf "$SRC.tmp"
	retry git clone -q --depth 1 -b $BR_VERSION https://gitlab.com/buildroot.org/buildroot.git "$SRC.tmp"
	mv "$SRC.tmp" "$SRC"
fi

[ -n "$CLEAN" ] && rm -rf "$OUT"
BRMAKE="make -C $SRC O=$OUT BR2_EXTERNAL=$HERE/buildroot"

# (re)configure when the defconfig changed
if [ ! -f "$OUT/.config" ] || [ "$HERE/buildroot/configs/$DEFCONFIG" -nt "$OUT/.config" ]; then
	$BRMAKE $DEFCONFIG
fi

# Buildroot copies local sources only once: re-sync the Rust workspace
if ls "$OUT"/build/canlogger-*/.stamp_rsynced >/dev/null 2>&1; then
	$BRMAKE -s canlogger-clean-for-rebuild
fi

retry $BRMAKE -j"$JOBS" "$@"

if [ -x "$OUT/host/bin/ccache" ]; then
	CCACHE_DIR=$BR2_CCACHE_DIR "$OUT/host/bin/ccache" -s | awk '/Hits|Misses/ && !seen[$1]++' || true
fi
if [ $# -eq 0 ] && [ -z "$CANLOGGER_IN_DOCKER" ]; then
	echo
	echo "Image: $OUT/images/sdcard.img"
	echo "Flash: sudo dd if=$OUT/images/sdcard.img of=/dev/sdX bs=4M conv=fsync status=progress"
fi
