# Helpers for local.conf (sourced by post-build.sh and post-image.sh).
LOCAL_CONF="$(dirname "$0")/local.conf"

# local_conf KEY -> value from local.conf, empty if unset
local_conf() {
	[ -f "$LOCAL_CONF" ] || return 0
	sed -n "s/^[[:space:]]*$1[[:space:]]*=\(.*\)$/\1/p" "$LOCAL_CONF" | tail -n 1 |
		sed 's/\r$//; s/^[[:space:]]*//; s/[[:space:]]*$//; s/^"\(.*\)"$/\1/'
}

# local.conf without build-only keys, for appending to logger.conf
local_conf_runtime() {
	[ -f "$LOCAL_CONF" ] || return 0
	grep -v -E '^[[:space:]]*root_password[[:space:]]*=' "$LOCAL_CONF" | sed 's/\r$//'
}
