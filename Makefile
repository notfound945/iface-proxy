BIN := iface-socks5
CARGO := cargo
TARGET_DIR := target
RELEASE_DIR := $(TARGET_DIR)/release
LINUX_TARGET := x86_64-unknown-linux-musl

# 可覆盖：make run IFACE=en0
IFACE ?= en0

.PHONY: all help build release run run-release strip clean linux-musl

all: release

help:
	@echo "Targets:"
	@echo "  build         - Debug build"
	@echo "  release       - Release build"
	@echo "  run           - Run debug with --iface=$(IFACE)"
	@echo "  run-release   - Run release with --iface=$(IFACE)"
	@echo "  strip         - Strip release binary (macOS)"
	@echo "  linux-musl    - Build static Linux musl binary"
	@echo "  clean         - Clean cargo artifacts"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

run: build
	$(CARGO) run -- --iface $(IFACE)

run-release: release
	$(RELEASE_DIR)/$(BIN) --iface $(IFACE)

strip: release
	strip -x $(RELEASE_DIR)/$(BIN)

linux-musl:
	rustup target add $(LINUX_TARGET) || true
	$(CARGO) build --release --target $(LINUX_TARGET)
	@echo "Binary: $(TARGET_DIR)/$(LINUX_TARGET)/release/$(BIN)"

clean:
	$(CARGO) clean


