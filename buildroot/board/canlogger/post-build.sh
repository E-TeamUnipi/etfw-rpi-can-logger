#!/bin/sh
# Adjust the target rootfs for a read-only root with our own init.
set -e

# our inittab starts everything; the packages' SysV scripts are not used
rm -f "${TARGET_DIR}"/etc/init.d/S*

# dropbear: keep the host key on the rootfs (generated on first boot)
if [ -L "${TARGET_DIR}/etc/dropbear" ]; then
	rm -f "${TARGET_DIR}/etc/dropbear"
fi
mkdir -p "${TARGET_DIR}/etc/dropbear"

# fixed machine id (dbus needs one; the rootfs is read-only)
if [ ! -s "${TARGET_DIR}/etc/machine-id" ]; then
	od -An -N16 -tx1 /dev/urandom | tr -d ' \n' > "${TARGET_DIR}/etc/machine-id"
	echo >> "${TARGET_DIR}/etc/machine-id"
fi

mkdir -p "${TARGET_DIR}/boot" "${TARGET_DIR}/var/lib"
