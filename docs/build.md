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
| `make build` | `cargo build --locked --release`, copies the binary to `build/cc-gigabyte-gpu-rgb` |
| `make test` | `cargo test --locked` |
| `make lint` | `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings` |
| `make check` | `test` + `lint` |
| `make fmt` | `cargo fmt` |
| `make image` | builds the build container image (done by the targets above) |
| `make clean` | removes `build/` and `target/`, and the target-cache volume |
| `sudo make install` / `sudo make uninstall` | see the [README](../README.md#install) |

By default every cargo command runs in a container built from
[`Containerfile.build`](../Containerfile.build): `fedora:45` with cargo, rust, clippy,
rustfmt, gcc and protobuf-compiler. The repo is mounted at `/src` with `:z` (SELinux).
The cargo registry and the target directory live in the named volumes
`cc-gigabyte-gpu-rgb-cargo` and `cc-gigabyte-gpu-rgb-target`, so rebuilds are fast.
Variables: `PODMAN`, `IMAGE`, `CARGO_VOL`, `TARGET_VOL`.

The binary is linked against the container's glibc (Fedora 45). It runs on Fedora 45 or
anything with a glibc at least as new. On another distribution, use `CONTAINER=0` or
change the image.

Without a container:

```sh
make CONTAINER=0 build      # or: cargo build --locked --release
make CONTAINER=0 check
```

`Cargo.lock` is committed and every target uses `--locked`. To change dependencies, run
`cargo update` (or a build without `--locked`) once and commit the new lock file.

## Building for install

```sh
make build              # as your user, not as root
sudo make install       # installs build/cc-gigabyte-gpu-rgb; never builds
```

`install` refuses to run without `build/cc-gigabyte-gpu-rgb` and does not start Podman
as root. `DESTDIR=...` stages the files into a directory without needing root or
touching systemd; that is how the install target is tested:

```sh
make install DESTDIR=/tmp/stage
make install DESTDIR=/tmp/stage MANIFEST_PLUGINS_DIR=/etc/coolercontrol/plugins
make uninstall DESTDIR=/tmp/stage
```

`MANIFEST_PLUGINS_DIR` must be an absolute path. The old `PLUGINS_DIR` variable is
rejected.

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
| `tests/grpc.rs` | end-to-end tests over a real Unix socket with a tonic client and the mock bus |
| `tests/autosave.rs` | auto-save behaviour with the mock bus and paused tokio time |
| `proto/` | CoolerControl gRPC API, vendored unmodified from `cc-plugins` commit `47edcf3a` |
| `dist/` | systemd unit, plugin manifest, modules-load file |
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
