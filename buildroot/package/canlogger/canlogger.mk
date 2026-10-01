################################################################################
#
# canlogger
#
################################################################################

CANLOGGER_VERSION = 0.1.0
# the Rust workspace is the parent directory of this BR2_EXTERNAL tree
CANLOGGER_SITE = $(BR2_EXTERNAL_CANLOGGER_PATH)/..
CANLOGGER_SITE_METHOD = local
CANLOGGER_LICENSE = MIT
CANLOGGER_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = \
	--exclude target --exclude buildroot --exclude VENDOR --exclude .cargo

CANLOGGER_BINS = canlogd canweb canring

ifeq ($(BR2_PACKAGE_CANLOGGER_BLE),y)
CANLOGGER_DEPENDENCIES += dbus host-pkgconf
CANLOGGER_BINS += canble
# libdbus-sys finds libdbus through Buildroot's pkg-config wrapper
CANLOGGER_CARGO_ENV += PKG_CONFIG_ALLOW_CROSS=1
else
CANLOGGER_CARGO_BUILD_OPTS += --workspace --exclude canble
endif

# Buildroot vendors crates for downloaded cargo packages only. This package
# is local, so vendor right before building (needs network once; crates are
# cached in $(DL_DIR)/br-cargo-home).
define CANLOGGER_CARGO_VENDOR
	cd $(@D) && mkdir -p .cargo && \
	$(HOST_MAKE_ENV) CARGO_HOME=$(BR_CARGO_HOME) \
		cargo vendor --locked VENDOR > .cargo/config.toml
endef
CANLOGGER_PRE_BUILD_HOOKS += CANLOGGER_CARGO_VENDOR

# workspace: install the binaries directly instead of "cargo install"
define CANLOGGER_INSTALL_TARGET_CMDS
	$(foreach b,$(CANLOGGER_BINS), \
		$(INSTALL) -D -m 0755 \
			$(@D)/target/$(RUSTC_TARGET_NAME)/$(if $(BR2_ENABLE_DEBUG),debug,release)/$(b) \
			$(TARGET_DIR)/usr/bin/$(b)
	)
endef

$(eval $(cargo-package))
