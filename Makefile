# SPDX-License-Identifier: GPL-3.0-or-later
#
# Building and testing run inside a Podman container (see Containerfile.build),
# so the host needs only podman. Set CONTAINER=0 to use a local cargo + protoc.
#
#   make build     release binary -> build/cc-gigabyte-gpu-rgb
#   make test      cargo test
#   make lint      cargo fmt --check + cargo clippy -D warnings
#   make check     test + lint
#   sudo make install / sudo make uninstall   (no build, no podman as root)
#   sudo make install MANIFEST_PLUGINS_DIR=/etc/coolercontrol/plugins
#                  for the official CoolerControl container image (see README);
#                  pass the same value to uninstall.

.DEFAULT_GOAL := build

NAME        := cc-gigabyte-gpu-rgb
PLUGIN_ID   := gigabyte-gpu-rgb

# --- build environment ---------------------------------------------------------
CONTAINER   ?= 1
PODMAN      ?= podman
IMAGE       ?= localhost/$(NAME)-build:latest
CARGO_VOL   ?= $(NAME)-cargo
TARGET_VOL  ?= $(NAME)-target

ifeq ($(CONTAINER),1)
RUN := $(PODMAN) run --rm --init \
	-v "$(CURDIR)":/src:z \
	-v $(CARGO_VOL):/root/.cargo/registry \
	-v $(TARGET_VOL):/target \
	-e CARGO_TARGET_DIR=/target \
	-w /src $(IMAGE)
TARGET_DIR := /target
else
RUN :=
TARGET_DIR := target
endif

# --- install locations ---------------------------------------------------------
DESTDIR     ?=
BINDIR      ?= /usr/local/bin
UNITDIR     ?= /etc/systemd/system
MODLOADDIR  ?= /etc/modules-load.d

# Where the service creates its socket. Fixed: the same path is hardcoded in
# dist/cc-gigabyte-gpu-rgb.service (--socket, ReadWritePaths=), dist/manifest.toml
# (address) and DEFAULT_SOCKET in src/lib.rs.
SOCKET_DIR  := /var/lib/coolercontrol/plugins/$(PLUGIN_ID)

# Auto-save records. Fixed: StateDirectory= in the unit and DEFAULT_STATE_DIR in
# src/autosave.rs. systemd creates it; uninstall removes it.
STATE_DIR   := /var/lib/$(NAME)

# The plugins directory the CoolerControl daemon scans for manifests. Native
# packages use /var/lib/coolercontrol/plugins; the official container image sets
# CC_DATA_DIR=/etc/coolercontrol and so scans /etc/coolercontrol/plugins.
MANIFEST_PLUGINS_DIR ?= /var/lib/coolercontrol/plugins
MANIFEST_DIR := $(MANIFEST_PLUGINS_DIR)/$(PLUGIN_ID)

ifdef PLUGINS_DIR
$(error PLUGINS_DIR is no longer used; set MANIFEST_PLUGINS_DIR (where the manifest goes) instead)
endif

# Refuses an empty or relative MANIFEST_PLUGINS_DIR before anything is installed or removed.
CHECK_DIRS = case "$(MANIFEST_PLUGINS_DIR)" in /?*) ;; *) echo "MANIFEST_PLUGINS_DIR must be an absolute path"; exit 1;; esac

.PHONY: image build test lint fmt check clean install uninstall

image:
ifeq ($(CONTAINER),1)
	$(PODMAN) build -t $(IMAGE) -f Containerfile.build .
endif

build: image
	$(RUN) cargo build --locked --release
	mkdir -p build
ifeq ($(CONTAINER),1)
	$(RUN) cp $(TARGET_DIR)/release/$(NAME) /src/build/$(NAME)
else
	cp $(TARGET_DIR)/release/$(NAME) build/$(NAME)
endif
	@echo "Built build/$(NAME)"

test: image
	$(RUN) cargo test --locked

lint: image
	$(RUN) cargo fmt --check
	$(RUN) cargo clippy --locked --all-targets -- -D warnings

fmt: image
	$(RUN) cargo fmt

check: test lint

clean:
	rm -rf build target
ifeq ($(CONTAINER),1)
	-$(PODMAN) volume rm $(TARGET_VOL)
endif

# Installs the already built binary. Run `make build` first as your normal user.
# Idempotent: re-running replaces the files and restarts the service.
install:
	@if [ "$$(id -u)" != 0 ] && [ -z "$(DESTDIR)" ]; then echo "install must run as root (sudo make install)"; exit 1; fi
	@if [ ! -x build/$(NAME) ]; then echo "build/$(NAME) not found; run 'make build' first (not as root)"; exit 1; fi
	@$(CHECK_DIRS)
	install -D -m 0755 build/$(NAME) $(DESTDIR)$(BINDIR)/$(NAME)
	install -D -m 0644 dist/$(NAME).service $(DESTDIR)$(UNITDIR)/$(NAME).service
	install -D -m 0644 dist/modules-load.conf $(DESTDIR)$(MODLOADDIR)/$(NAME).conf
	install -d -m 0755 $(DESTDIR)$(SOCKET_DIR)
	install -d -m 0755 $(DESTDIR)$(MANIFEST_DIR)
	install -m 0644 dist/manifest.toml $(DESTDIR)$(MANIFEST_DIR)/manifest.toml
	@if [ -z "$(DESTDIR)" ]; then \
		if command -v restorecon >/dev/null 2>&1; then \
			restorecon -F $(BINDIR)/$(NAME) $(UNITDIR)/$(NAME).service $(MODLOADDIR)/$(NAME).conf || true; \
			restorecon -RF $(SOCKET_DIR) $(MANIFEST_DIR) || true; \
		fi; \
		modprobe i2c-dev || true; \
		systemctl daemon-reload; \
		systemctl enable $(NAME).service; \
		systemctl restart $(NAME).service; \
		echo "Installed. Manifest: $(MANIFEST_DIR)/manifest.toml, socket directory: $(SOCKET_DIR)."; \
		echo "Now restart CoolerControl (coolercontrold.service, or your container unit)."; \
	fi

uninstall:
	@if [ "$$(id -u)" != 0 ] && [ -z "$(DESTDIR)" ]; then echo "uninstall must run as root"; exit 1; fi
	@$(CHECK_DIRS)
	-@if [ -z "$(DESTDIR)" ]; then systemctl disable --now $(NAME).service; fi
	rm -f $(DESTDIR)$(UNITDIR)/$(NAME).service
	rm -f $(DESTDIR)$(BINDIR)/$(NAME)
	rm -f $(DESTDIR)$(MODLOADDIR)/$(NAME).conf
	rm -rf $(DESTDIR)$(MANIFEST_DIR)
	rm -rf $(DESTDIR)$(SOCKET_DIR)
	rm -rf $(DESTDIR)$(STATE_DIR)
	-@if [ -z "$(DESTDIR)" ]; then systemctl daemon-reload; fi
	@echo "Uninstalled. Restart CoolerControl so it forgets the plugin."
