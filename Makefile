# SPDX-License-Identifier: GPL-3.0-or-later
#
# Building and testing run inside a Podman container (see Containerfile.build),
# so the host needs only podman. Set CONTAINER=0 to use a local cargo + protoc.
#
#   make build     release binary -> build/cc-gigabyte-gpu-rgb
#   make test      cargo test + install.sh tests (DESTDIR, no root)
#   make lint      cargo fmt --check + cargo clippy -D warnings
#   make check     test + lint (incl. shellcheck)
#   make dist      static musl release archive -> build/dist/ (as published by CI)
#   sudo make install / sudo make uninstall   via install.sh (no build, no podman as root)
#   sudo make install MANIFEST_PLUGINS_DIR=DIR   override the manifest location

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

# --- release archive (make dist) -----------------------------------------------
# A static musl binary plus everything needed to install it, as published by CI.
MUSL        := x86_64-unknown-linux-musl
VERSION     := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
DIST_NAME   := $(NAME)-$(VERSION)-x86_64-linux
DIST_OUT    := build/dist
DIST_IMAGE  ?= localhost/$(NAME)-dist:latest
DIST_VOL    ?= $(NAME)-dist-target
SOURCE_DATE_EPOCH ?= $(shell git log -1 --format=%ct 2>/dev/null || date +%s)

ifeq ($(CONTAINER),1)
DIST_RUN := $(PODMAN) run --rm --init \
	-v "$(CURDIR)":/src:z \
	-v $(CARGO_VOL):/opt/cargo/registry \
	-v $(DIST_VOL):/target \
	-e CARGO_TARGET_DIR=/target \
	-w /src $(DIST_IMAGE)
DIST_TARGET_DIR := /target
else
DIST_RUN :=
DIST_TARGET_DIR := target
endif

# --- install (delegated to install.sh) -------------------------------------------
# MANIFEST_PLUGINS_DIR: CoolerControl plugins directory for the manifest. Empty means
# install.sh chooses (and prints) it. DESTDIR stages without root or systemd.
DESTDIR     ?=
MANIFEST_PLUGINS_DIR ?=

ifdef PLUGINS_DIR
$(error PLUGINS_DIR is no longer used; set MANIFEST_PLUGINS_DIR (where the manifest goes) instead)
endif

INSTALL_SH = BINARY="$(CURDIR)/build/$(NAME)" DESTDIR="$(DESTDIR)" \
	MANIFEST_PLUGINS_DIR="$(MANIFEST_PLUGINS_DIR)" ./install.sh

.PHONY: image dist-image build dist test lint fmt check clean install uninstall

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

dist-image:
ifeq ($(CONTAINER),1)
	$(PODMAN) build -t $(DIST_IMAGE) -f Containerfile.dist .
endif

# build/dist/$(DIST_NAME).tar.gz and .sha256
dist: dist-image
	@test -n "$(VERSION)" || { echo "cannot read version from Cargo.toml"; exit 1; }
	$(DIST_RUN) cargo build --locked --release --target $(MUSL)
	rm -rf build/dist
	mkdir -p $(DIST_OUT)/$(DIST_NAME)
ifeq ($(CONTAINER),1)
	$(DIST_RUN) cp $(DIST_TARGET_DIR)/$(MUSL)/release/$(NAME) /src/$(DIST_OUT)/$(DIST_NAME)/$(NAME)
else
	cp $(DIST_TARGET_DIR)/$(MUSL)/release/$(NAME) $(DIST_OUT)/$(DIST_NAME)/$(NAME)
endif
	cp -R install.sh README.md LICENSE dist docs $(DIST_OUT)/$(DIST_NAME)/
	cd $(DIST_OUT) && tar --sort=name --owner=0 --group=0 --numeric-owner \
		--mtime=@$(SOURCE_DATE_EPOCH) -cf - $(DIST_NAME) | gzip -n -9 > $(DIST_NAME).tar.gz
	cd $(DIST_OUT) && sha256sum $(DIST_NAME).tar.gz > $(DIST_NAME).tar.gz.sha256
	@echo "Built $(DIST_OUT)/$(DIST_NAME).tar.gz"

test: image
	$(RUN) cargo test --locked
	$(RUN) sh tests/install_sh.sh

lint: image
	$(RUN) cargo fmt --check
	$(RUN) cargo clippy --locked --all-targets -- -D warnings
	$(RUN) shellcheck -s sh install.sh tests/install_sh.sh

fmt: image
	$(RUN) cargo fmt

check: test lint

clean:
	rm -rf build target
ifeq ($(CONTAINER),1)
	-$(PODMAN) volume rm $(TARGET_VOL)
endif

# Installs the already built binary (run `make build` first, as your user) through
# install.sh, the single implementation shared with the release archive.
install:
	@if [ ! -x build/$(NAME) ]; then echo "build/$(NAME) not found; run 'make build' first (not as root)"; exit 1; fi
	$(INSTALL_SH)

uninstall:
	$(INSTALL_SH) --uninstall
