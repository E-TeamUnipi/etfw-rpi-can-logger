#!/bin/sh
# Adjust the target rootfs for a read-only root with our own init.
set -e
. "$(dirname "$0")/local-conf.sh"

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

# root password from local.conf (kept out of git). Without one, root has no
# usable password and SSH needs a key from local authorized_keys.
ROOT_PW=$(local_conf root_password)
if [ -n "$ROOT_PW" ]; then
	OPENSSL=openssl
	[ -x "${HOST_DIR}/bin/openssl" ] && OPENSSL="${HOST_DIR}/bin/openssl"
	HASH=$(printf '%s' "$ROOT_PW" | "$OPENSSL" passwd -6 -stdin)
else
	HASH='*'
	echo "post-build: no root_password in local.conf, root password login disabled" >&2
fi
HASH_ESC=$(printf '%s' "$HASH" | sed 's/[\/&]/\\&/g')
sed -i "s/^root:[^:]*:/root:${HASH_ESC}:/" "${TARGET_DIR}/etc/shadow"

KEYS="$(dirname "$0")/authorized_keys"
rm -f "${TARGET_DIR}/root/.ssh/authorized_keys"
if [ -f "$KEYS" ]; then
	install -D -m 0600 "$KEYS" "${TARGET_DIR}/root/.ssh/authorized_keys"
	chmod 0700 "${TARGET_DIR}/root/.ssh"
fi
