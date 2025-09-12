BIN := iface-socks5
CARGO := cargo
TARGET_DIR := target
RELEASE_DIR := $(TARGET_DIR)/release
LINUX_TARGET := x86_64-unknown-linux-musl

# 可覆盖：make run IFACE=en0 LISTEN=127.0.0.1:7891 SOCKS5=127.0.0.1:1080 USER=foo PASS=bar
IFACE ?= en0
LISTEN ?= 127.0.0.1:7890
SOCKS5 ?=
USER ?=
PASS ?=

.PHONY: all help build release run run-release strip clean linux-musl

all: release

help:
	@echo "Targets:"
	@echo "  build         - Debug build"
	@echo "  release       - Release build"
	@echo "  run           - Run debug (iface/listen/socks5/user/pass env supported)"
	@echo "  run-release   - Run release (iface/listen/socks5/user/pass env supported)"
	@echo "  strip         - Strip release binary (macOS)"
	@echo "  linux-musl    - Build static Linux musl binary"
	@echo "  clean         - Clean cargo artifacts"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

run: build
	$(CARGO) run -- --iface $(IFACE) --listen $(LISTEN) $(if $(SOCKS5),--socks5-listen $(SOCKS5)) $(if $(USER),--socks5-user $(USER)) $(if $(PASS),--socks5-pass $(PASS))

run-release: release
	$(RELEASE_DIR)/$(BIN) --iface $(IFACE) --listen $(LISTEN) $(if $(SOCKS5),--socks5-listen $(SOCKS5)) $(if $(USER),--socks5-user $(USER)) $(if $(PASS),--socks5-pass $(PASS))

strip: release
	strip -x $(RELEASE_DIR)/$(BIN)

linux-musl:
	rustup target add $(LINUX_TARGET) || true
	$(CARGO) build --release --target $(LINUX_TARGET)
	@echo "Binary: $(TARGET_DIR)/$(LINUX_TARGET)/release/$(BIN)"

clean:
	$(CARGO) clean


