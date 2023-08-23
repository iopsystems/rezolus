
include /usr/share/dpkg/buildtools.mk
include debian/cargo/rustc-architecture.mk

export DEB_HOST_RUST_TYPE DEB_BUILD_RUST_TYPE DEB_TARGET_RUST_TYPE

ifneq ($(DEB_HOST_GNU_TYPE), $(DEB_BUILD_GNU_TYPE))

# The pkg-config crate needs this to be set otherwise it will refuse to allow
# any cross compiling.
export PKG_CONFIG_ALLOW_CROSS := 1

# The clang driver used by bindgen uses native headers unless you explicitly set
# the sysroot so we do it here.
export BINDGEN_EXTRA_CLANG_ARGS := --sysroot /usr/$(DEB_HOST_GNU_TYPE)

endif

export TARGET_AR         := $(AR)
export TARGET_CC         := $(CC)
export TARGET_CXX        := $(CXX)
export TARGET_PKG_CONFIG := $(PKG_CONFIG)

export HOST_AR           := $(AR_FOR_BUILD)
export HOST_CC           := $(CC_FOR_BUILD)
export HOST_CXX          := $(CXX_FOR_BUILD)
export HOST_PKG_CONFIG   := $(PKG_CONFIG_FOR_BUILD)
