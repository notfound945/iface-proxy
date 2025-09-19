BIN := iface-proxy
CARGO := cargo
TARGET_DIR := target
RELEASE_DIR := $(TARGET_DIR)/release

# 可覆盖：make run IFACE=en0 LISTEN=127.0.0.1:7890 SOCKS5=1 or SOCKS5=127.0.0.1:7081 USER=foo PASS=bar
IFACE ?= en0
LISTEN ?= 127.0.0.1:7890
SOCKS5 ?=
USER ?=
PASS ?=
NO_SOCKS5 ?=

.PHONY: all help build release run run-release strip clean \
    	stress-build stress stress-http stress-connect stress-idle

all: release

help:
	@echo "Targets:"
	@echo "  build         - Debug build"
	@echo "  release       - Release build"
	@echo "  run           - Run debug (iface/listen/socks5/user/pass env)"
	@echo "  run-release   - Run release (iface/listen/socks5/user/pass env)"
	@echo "  stress-build  - Build stress tool (release)"
	@echo "  stress        - Run stress (vars: STRESS_TARGET/STRESS_MODE/STRESS_PAYLOAD/STRESS_CONNS/STRESS_DURATION)"
	@echo "  stress-http   - Run HTTP stress (override vars as needed)"
	@echo "  stress-connect- Run CONNECT stress (override vars as needed)"
	@echo "  stress-idle   - Run idle-conn stress (override vars as needed)"
	@echo "  strip         - Strip release binary (macOS)"
	@echo "  clean         - Clean cargo artifacts"

build:
	$(CARGO) build

release:
	$(CARGO) build --release

run: build
	$(CARGO) run --bin $(BIN) -- --iface $(IFACE) --listen $(LISTEN) $(if $(SOCKS5),--socks5) $(if $(filter 1,$(SOCKS5)),,$(if $(SOCKS5),--socks5-listen $(SOCKS5))) $(if $(USER),--socks5-user $(USER)) $(if $(PASS),--socks5-pass $(PASS))

run-release: release
	$(RELEASE_DIR)/$(BIN) --iface $(IFACE) --listen $(LISTEN) $(if $(SOCKS5),--socks5) $(if $(filter 1,$(SOCKS5)),,$(if $(SOCKS5),--socks5-listen $(SOCKS5))) $(if $(USER),--socks5-user $(USER)) $(if $(PASS),--socks5-pass $(PASS))

# ------------ Stress tool -------------
STRESS_TARGET ?= 127.0.0.1:7890
STRESS_MODE ?= http
STRESS_PAYLOAD ?= http://example.com/
STRESS_CONNS ?= 1000
STRESS_DURATION ?= 120

stress-build:
	$(CARGO) build --release --bin stress

stress: stress-build
	$(RELEASE_DIR)/stress --target $(STRESS_TARGET) --mode $(STRESS_MODE) --payload $(STRESS_PAYLOAD) --conns $(STRESS_CONNS) --duration-secs $(STRESS_DURATION)

stress-http:
	$(MAKE) stress STRESS_MODE=http

stress-connect:
	$(MAKE) stress STRESS_MODE=connect STRESS_PAYLOAD=example.com:443

stress-idle:
	$(MAKE) stress STRESS_MODE=idle

strip: release
	strip -x $(RELEASE_DIR)/$(BIN)

 

clean:
	$(CARGO) clean


