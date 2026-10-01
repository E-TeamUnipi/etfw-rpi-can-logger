#!/bin/bash
# Assemble sdcard.img: FAT boot partition (firmware, kernel, DTBs, overlays,
# config.txt, cmdline.txt, logger.conf) + read-only ext4 rootfs.
set -e

BOARD_DIR="$(dirname "$0")"
. "${BOARD_DIR}/local-conf.sh"

# logger.conf on the boot partition: the repo's defaults plus local.conf
# (later keys win). Regenerated on every run so overrides don't pile up.
CONF_OUT="${BINARIES_DIR}/rpi-firmware/logger.conf"
mkdir -p "${BINARIES_DIR}/rpi-firmware"
cp "${BOARD_DIR}/logger.conf" "${CONF_OUT}"
if [ -f "${LOCAL_CONF}" ]; then
	{ echo; echo "# --- local overrides (local.conf at build time) ---"; local_conf_runtime; } >> "${CONF_OUT}"
fi
GENIMAGE_CFG="${BINARIES_DIR}/genimage.cfg"
GENIMAGE_TMP="${BUILD_DIR}/genimage.tmp"

FILES=()
for i in "${BINARIES_DIR}"/*.dtb "${BINARIES_DIR}"/rpi-firmware/*; do
	FILES+=( "${i#${BINARIES_DIR}/}" )
done
FILES+=( "Image" )

BOOT_FILES=$(printf '\\t\\t\\t"%s",\\n' "${FILES[@]}")
sed "s|#BOOT_FILES#|${BOOT_FILES}|" "${BOARD_DIR}/genimage.cfg.in" > "${GENIMAGE_CFG}"

trap 'rm -rf "${ROOTPATH_TMP}"' EXIT
ROOTPATH_TMP="$(mktemp -d)"
rm -rf "${GENIMAGE_TMP}"

genimage \
	--rootpath "${ROOTPATH_TMP}" \
	--tmppath "${GENIMAGE_TMP}" \
	--inputpath "${BINARIES_DIR}" \
	--outputpath "${BINARIES_DIR}" \
	--config "${GENIMAGE_CFG}"

echo
echo "SD card image: ${BINARIES_DIR}/sdcard.img"
