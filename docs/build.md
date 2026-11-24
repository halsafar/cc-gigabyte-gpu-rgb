# Building and contributing

## Requirements

- With the default container build: Podman (rootless is fine) and `make`. Nothing else
  on the host.
- With a local toolchain (`CONTAINER=0`): Rust 1.86 or newer and `protoc`
  (protobuf-compiler). `build.rs` generates the gRPC code from [`proto/`](../proto) at
  build time.

## Make targets

| Target | What it does |
|---|---|
| `make build` | `cargo build --locked --release` (glibc, for local use), copies the binary to `build/cc-gigabyte-gpu-rgb` |
| `make dist` | static musl release archive in `build/dist/` (below) |
| `make test` | `cargo test --locked` and `tests/install_sh.sh` |
| `make lint` | `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `shellcheck` on the shell scripts |
| `make check` | `test` + `lint` |
| `make fmt` | `cargo fmt` |
| `make image` / `make dist-image` | build the container images (done by the targets above) |
| `make clean` | removes `build/` and `target/`, and the target-cache volume |
| `sudo make install` / `sudo make uninstall` | runs `install.sh` with `build/cc-gigabyte-gpu-rgb` |

By default every cargo command runs in a container built from
[`Containerfile.build`](../Containerfile.build): `fedora:45` with cargo, rust, clippy,
rustfmt, gcc, protobuf-compiler and ShellCheck. The repo is mounted at `/src` with `:z`
(SELinux). The cargo registry and the target directory live in the named volumes
`cc-gigabyte-gpu-rgb-cargo` and `cc-gigabyte-gpu-rgb-target`, so rebuilds are fast.
Variables: `PODMAN`, `IMAGE`, `CARGO_VOL`, `TARGET_VOL`.

The `make build` binary is linked against the container's glibc (Fedora 45). It runs on
Fedora 45 or anything with a glibc at least as new. For a binary that runs anywhere, use
`make dist`.

Without a container:

```sh
make CONTAINER=0 build      # or: cargo build --locked --release
make CONTAINER=0 check
make CONTAINER=0 dist       # needs rustup with the musl target (rust-toolchain.toml)
```

`Cargo.lock` is committed and every target uses `--locked`. To change dependencies, run
`cargo update` (or a build without `--locked`) once and commit the new lock file.

## Release archive (`make dist`)

`make dist` builds `--target x86_64-unknown-linux-musl`. That gives a static-pie
binary with no glibc or libgcc dependency, so it runs on any x86-64 Linux. All
dependencies are pure Rust plus libc. The build runs in a second container,
[`Containerfile.dist`](../Containerfile.dist) (`fedora:45` with rustup, because Fedora's
Rust packages lack the musl target), using the toolchain pinned in
[`rust-toolchain.toml`](../rust-toolchain.toml), the same one CI uses. Its target cache
is the volume `cc-gigabyte-gpu-rgb-dist-target`.

Output, for version `V` from `Cargo.toml`:

```
build/dist/cc-gigabyte-gpu-rgb-V-x86_64-linux.tar.gz
build/dist/cc-gigabyte-gpu-rgb-V-x86_64-linux.tar.gz.sha256
```

The archive contains one directory `cc-gigabyte-gpu-rgb-V-x86_64-linux/` with the binary,
`install.sh`, `dist/` (unit, manifest, modules-load file), `README.md`, `LICENSE` and
`docs/`. It is reproducible: sorted entries, owner 0/0, mtime from the last commit
(`SOURCE_DATE_EPOCH`), `gzip -n`. Two runs from the same commit give the same checksum.

## Installing

`install.sh` is the only install implementation. The release archive runs it directly
(`sudo ./install.sh`). The Makefile calls it with `BINARY=build/cc-gigabyte-gpu-rgb`:

```sh
make build              # as your user, not as root
sudo make install       # never builds, never starts Podman as root
```

Options and environment: `--uninstall`; `--manifest-dir DIR` or `MANIFEST_PLUGINS_DIR`
(must be absolute); `DESTDIR` stages into a directory without root or systemd; `BINARY`
sets the binary to install. The old `PLUGINS_DIR` variable is rejected by the Makefile.
How the manifest directory is chosen is described in
[design.md](design.md#how-installsh-picks-the-manifest-directory).

Staging by hand:

```sh
make install DESTDIR=/tmp/stage
make install DESTDIR=/tmp/stage MANIFEST_PLUGINS_DIR=/etc/coolercontrol/plugins
make uninstall DESTDIR=/tmp/stage
```

`tests/install_sh.sh` runs these cases automatically against fake trees:

- native default
- container (real `/etc/coolercontrol/plugins`, plus a stale manifest)
- native symlink (an absolute link that must not escape `DESTDIR`)
- override
- refusals

It runs in `make test` and in CI.

## CI and releases

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on pushes to `main`, on
pull requests and on tags `v*`:

- **check** (every run):
  1. install protoc (apt) and the pinned toolchain (rustup is preinstalled on the
     runner)
  2. `cargo fmt --check`, clippy with `-D warnings`, `cargo test`, all `--locked`
  3. shellcheck and `tests/install_sh.sh`
  4. `make dist CONTAINER=0`, a check that the binary is static
  5. upload the archive and checksum as the workflow artifact `release-archive`, so
     every build can be downloaded
- **release** (tags only, after check):
  1. fails unless the tag minus `v` equals the version in `Cargo.toml` *and* in
     `dist/manifest.toml`
  2. downloads the artifact, verifies the checksum
  3. runs `gh release create <tag> --verify-tag --generate-notes` with both files. Only
     this job has `contents: write`; everything else is read-only.

Superseded runs on the same branch are cancelled; tag runs are never cancelled. Cargo
downloads and `target/` are cached with `actions/cache`, keyed on `Cargo.lock` and
`rust-toolchain.toml`.

Actions are pinned by full commit SHA, with the version in a comment. Tags can be moved
by whoever controls the action's repository; a SHA cannot. Only GitHub's own actions are
used (checkout, cache, upload-artifact, download-artifact).
[`.github/dependabot.yml`](../.github/dependabot.yml) proposes monthly updates.

### Cutting a release

1. Bump `version` in `Cargo.toml` **and** `dist/manifest.toml` (the release job
   checks both). Update the lock file with `cargo update -p cc-gigabyte-gpu-rgb --offline`
   (or one build without `--locked`), run `make check`, and commit.
2. Optionally `make dist` and try the archive.
3. Tag and push:

   ```sh
   git tag -a v0.2.0 -m "v0.2.0"
   git push origin main v0.2.0
   ```

4. The workflow publishes the release with generated notes. Edit the notes on GitHub if
   needed.

To change the Rust version, edit `rust-toolchain.toml` (CI and `make dist`). The Fedora
container used by `make build`/`make check` has its own toolchain.

## Repository layout

| Path | Contents |
|---|---|
| `src/protocol.rs` | pure packet encoding: modes, speeds, colours, `encode`, `encode_save`, probe-reply check. No I/O. |
| `src/bus.rs` | the `I2cBus` trait; `LinuxI2cBus` (`I2C_RDWR` ioctl via `libc`, `--debug` hex logging); `probe`, `write_packets`; `bus::mock::MockBus` for tests |
| `src/cards.rs` | `SUPPORTED_CARDS` table, `PciIds`, `CardKey` |
| `src/discovery.rs` | finds the I2C bus of each supported card in sysfs (sysfs root injectable) |
| `src/service.rs` | `GpuDevice` (per-device lock, apply, `try_save`), the gRPC `DeviceService`, request validation, shutdown `Flusher` |
| `src/autosave.rs` | state-directory records (`StateStore`), debounced auto-save (`AutoSave`), `configure`, CLI record helpers |
| `src/server.rs` | Unix-socket serving, stale-socket removal, socket mode |
| `src/lib.rs` | generated proto modules, `open_device`, `detect_devices`, `DEFAULT_SOCKET` |
| `src/main.rs` | CLI (`serve`, `probe`, `set`, `save`), logging, signals |
| `install.sh` | installer / uninstaller (POSIX sh), used by the release archive and `make install` |
| `tests/install_sh.sh` | `install.sh` tests in `DESTDIR` trees |
| `tests/grpc.rs` | end-to-end tests over a real Unix socket with a tonic client and the mock bus |
| `tests/autosave.rs` | auto-save behaviour with the mock bus and paused tokio time |
| `proto/` | CoolerControl gRPC API, vendored unmodified from `cc-plugins` commit `47edcf3a` |
| `dist/` | systemd unit, plugin manifest, modules-load file |
| `Containerfile.build`, `Containerfile.dist`, `rust-toolchain.toml` | build environments (local; release/CI toolchain) |
| `.github/` | CI and release workflow, Dependabot config |
| `docs/` | this file, [protocol.md](protocol.md), [design.md](design.md), [adding-a-card.md](adding-a-card.md) |
| `build.rs` | proto code generation (server and client; the client is only used by tests) |

## Tests

Everything runs without hardware.

- **Unit tests** (in each `src/` module):
  - exact packet bytes and order for every mode
  - the shared `B0` packet for zones 0 and 1
  - no colour packet for colour cycle
  - `off` as static black
  - the speed mapping
  - a missing colour is an error
  - the save packet, and no `AA` ever coming out of `encode`
  - probe accept and reject (including the all-`AB` echo) and I/O errors
  - `i2c_msg` struct layout
  - sysfs discovery against a fake tree in a temp dir
  - stale-socket handling
  - request validation
  - record files: round trip, keying, corrupt records, path sanitising, CLI helpers
- **`tests/grpc.rs`**:
  - `ListDevices` shape
  - read-only RPCs never touch I2C
  - lighting packets over the wire
  - invalid requests rejected without I/O
  - bus failure gives `UNAVAILABLE`
  - zero devices
  - without auto-save, no save packet ever
  - with auto-save, a save only on a real change and never from other RPCs
- **`tests/autosave.rs`**, with paused time:
  - one save after the delay
  - clicking through settings costs one save
  - no save when re-applying the saved setting, including after a restart
  - equivalent requests do not save
  - failed save: no retry, record unchanged
  - a failed apply cancels the pending save
  - a corrupt record leads to one save
  - a record that cannot be written still saves
  - auto-save off and `configure`
  - shutdown flush
  - non-lighting RPCs stay silent
  - CLI record functions

## Smoke testing the binary without the card

```sh
build/cc-gigabyte-gpu-rgb probe                      # without access: clean permission error
build/cc-gigabyte-gpu-rgb --debug serve --socket ./test.sock --no-auto-save
```

The second command serves with zero devices if the bus cannot be opened. Unix socket
paths are limited to about 108 bytes, so use a short or relative path. A raw gRPC call
with curl:

```sh
printf '\0\0\0\0\0' > empty
curl -s --http2-prior-knowledge --unix-socket ./test.sock \
  -H 'content-type: application/grpc' -H 'te: trailers' --data-binary @empty \
  -D - -o /dev/null \
  http://localhost/coolercontrol.device_service.v1.DeviceService/Health
```

`grpc-status: 0` in the headers means success.

Check the unit with `systemd-analyze verify dist/cc-gigabyte-gpu-rgb.service`. It
complains if `/usr/local/bin/cc-gigabyte-gpu-rgb` does not exist; point `ExecStart=` at
`build/` in a copy to check it before installing.

## Licence of contributions

GPL-3.0-or-later. Keep the SPDX header on new files.
