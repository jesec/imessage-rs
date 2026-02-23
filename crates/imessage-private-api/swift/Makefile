# Build imessage-helper.dylib (arm64e, macOS 15+)
#
# This produces a single universal helper dylib that works for both
# Messages.app and FaceTime.app (it checks Bundle.main.bundleIdentifier).
#
# Pure Swift — no ObjC bridge needed.
#
# Usage:
#   make            — build the dylib
#   make clean      — remove build artifacts
#   make check      — verify output architecture

SHELL := /bin/bash

# Paths
ROOT          := $(shell dirname $(realpath $(lastword $(MAKEFILE_LIST))))
REPO_ROOT     := $(ROOT)/../../..
BUILD_DIR     := $(ROOT)/.build
OUT_DIR       := $(BUILD_DIR)
DYLIB         := $(OUT_DIR)/imessage-helper.dylib

# Sources
SWIFT_SRCS    := $(wildcard $(ROOT)/Sources/IMHelper/*.swift)

# Toolchain
SWIFTC        := xcrun swiftc

# Flags
MIN_MACOS     := 15.0
ARCH          := arm64e

SWIFT_FLAGS   := -target $(ARCH)-apple-macos$(MIN_MACOS) \
    -emit-library \
    -emit-module \
    -module-name IMHelper \
    -Xlinker -rpath -Xlinker @loader_path \
    -Xlinker -install_name -Xlinker @rpath/imessage-helper.dylib \
    -Xlinker -init -Xlinker __dylib_init \
    -framework Foundation \
    -framework Network \
    -framework CoreLocation \
    -framework CoreSpotlight \
    -framework Security

.PHONY: all clean check

all: $(DYLIB)

$(BUILD_DIR):
	@mkdir -p $(BUILD_DIR)

# Compile all Swift sources into the dylib
$(DYLIB): $(SWIFT_SRCS) | $(BUILD_DIR)
	$(SWIFTC) $(SWIFT_FLAGS) \
		$(SWIFT_SRCS) \
		-o $(DYLIB)
	codesign -fs - $(DYLIB)

clean:
	rm -rf $(BUILD_DIR)
	rm -f $(DYLIB)

check: $(DYLIB)
	@file $(DYLIB)
	@echo "---"
	@otool -L $(DYLIB) | head -20
