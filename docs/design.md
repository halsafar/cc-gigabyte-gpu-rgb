# Design and internals

How the plugin is put together and why. For the bytes on the wire see
[protocol.md](protocol.md); for building and the code layout see [build.md](build.md).

## Overview

- One binary, `cc-gigabyte-gpu-rgb`, run as a host systemd service. CoolerControl does
  not start it; the manifest has no `executable`, only an `address`. That is how a
  CoolerControl plugin says "managed by other means". It also lets the plugin work when
  the CoolerControl daemon runs in a container, which cannot create host systemd units.
- It serves CoolerControl's `coolercontrol.device_service.v1.DeviceService` gRPC API
  on a Unix socket. The proto files are vendored, unmodified, in [`proto/`](../proto).
- It opens one I2C bus per supported card and talks to one address. Nothing is polled.

## gRPC service

| RPC | Behaviour | Touches I2C |
|---|---|---|
| `Health` | name, version, uptime | no |
| `ListDevices` | one device per probed card (see below) | no |
| `InitializeDevice` | OK for a known device, `NOT_FOUND` otherwise | no |
| `Status` | always an empty list (no sensors) | no |
| `ResetChannel` | OK, does nothing | no |
| `Shutdown` | OK, logs, does **not** exit | no |
| `Lighting` | validates, encodes, writes; schedules auto-save | yes |
| fan, LCD, custom function | `UNIMPLEMENTED` | no |

`Shutdown` does not exit, because the service is run by systemd and must still be there
when the daemon starts again.

`ListDevices` reports, per card:

- `id` and `uid_info`: the PCI address, e.g. `0000:04:00.0`.
- `name` and `info.model`: the card name from the table in
  [`src/cards.rs`](../src/cards.rs).
- `info.channels`: one channel `gpu` (label `GPU`) with the lighting modes from
  [protocol.md](protocol.md#mode-bytes). `min_colors` = `max_colors` = 0 or 1, and
  `backward_enabled` = false.
- `info.lighting_speeds`: `slowest slower normal faster fastest`.
- `info.driver_info`: plugin name, version, and the device node and sysfs path as
  locations.

`Lighting` errors:

| Request | gRPC code |
|---|---|
| unknown device or channel | `NOT_FOUND` |
| missing setting, unknown mode, missing colour, colour value over 255, unknown speed on a mode with speed | `INVALID_ARGUMENT` |
| I2C write failed | `UNAVAILABLE` |

A speed sent with a mode that has no speed control is ignored, not validated. Extra
colours are ignored. `backward` is ignored, because the card has no direction control.
Rejected requests never reach the bus.

The I2C writes run in `spawn_blocking`, behind a per-device mutex.

## Startup and failure handling

1. Discover supported cards in sysfs (below).
2. Open `/dev/i2c-N` and probe each one.
3. Serve. If discovery or the probe fails, the reason is logged and the service keeps
   running with zero devices, so it does not crash-loop.

At startup a stale socket from an unclean exit is removed. A path that exists but is
not a socket is left alone and the service refuses to start. The socket is created with
mode `0600`, so only root can connect; the CoolerControl daemon runs as root, including
in a rootful container. On `SIGTERM` or `SIGINT` the server stops, any pending
auto-save is flushed, and the socket is removed.

Log level: `--debug` (which also logs every I2C transfer as hex), otherwise `CC_LOG`,
default `info`. Under journald, timestamps are left out.

## Command line

```
cc-gigabyte-gpu-rgb [--debug] [--sysfs-root DIR] [--state-dir DIR] [serve [--socket PATH]
                    [--auto-save-delay SECONDS] [--no-auto-save]]
cc-gigabyte-gpu-rgb [--debug] probe
cc-gigabyte-gpu-rgb [--debug] [--state-dir DIR] set <mode> [--color RRGGBB] [--speed NAME] [--save]
cc-gigabyte-gpu-rgb [--debug] [--state-dir DIR] save
```

| Command | What it does |
|---|---|
| (none) / `serve` | the gRPC service. Default socket `/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb/gigabyte-gpu-rgb.sock`. |
| `probe` | finds the card's bus, probes, prints the reply bytes |
| `set` | validates and encodes first (bad arguments never cause I/O), then probes and writes. `--save` also writes the save packet and records it. |
| `save` | probes, writes the save packet, clears the record |

`probe`, `set` and `save` use the first supported card if several are found.
`--sysfs-root` exists for testing against a fake sysfs tree.

## Bus discovery

I2C bus numbers are not stable across boots, so the bus is found through sysfs
(`discovery::discover`, with the sysfs root injectable for tests):

1. For every `/sys/bus/i2c/devices/i2c-<n>`, read `name`. Keep it if it starts with a
   table entry's `bus_name_prefix`.
2. Resolve the symlink. Walk up the real path to the first directory that has both
   `vendor` and `subsystem_device`: that is the PCI function. The walk never goes
   above the sysfs root.
3. Read `vendor`, `device`, `subsystem_vendor` and `subsystem_device`, and match them
   against the table.

On the RX 7700 XT GAMING OC the adapter directory sits directly under the GPU's PCI
function, e.g. `.../0000:03:00.0/0000:04:00.0/i2c-21`.

## Files and paths

| Path | What | Fixed in |
|---|---|---|
| `/usr/local/bin/cc-gigabyte-gpu-rgb` | binary | unit `ExecStart=` |
| `/etc/systemd/system/cc-gigabyte-gpu-rgb.service` | unit | |
| `/etc/modules-load.d/cc-gigabyte-gpu-rgb.conf` | loads `i2c-dev` at boot | |
| `/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb/gigabyte-gpu-rgb.sock` | socket | `dist/manifest.toml` (`address`), unit (`--socket`, `ReadWritePaths=`), `DEFAULT_SOCKET` in `src/lib.rs`, `SOCKET_DIR` in the Makefile |
| `<plugins dir>/gigabyte-gpu-rgb/manifest.toml` | manifest | `MANIFEST_PLUGINS_DIR` in the Makefile |
| `/var/lib/cc-gigabyte-gpu-rgb/` | auto-save records | unit `StateDirectory=`, `DEFAULT_STATE_DIR` in `src/autosave.rs`, `STATE_DIR` in the Makefile |

The binary is not put in the plugins directory. On SELinux systems files there are
labelled `var_lib_t`, which systemd cannot execute (`status=203/EXEC`).
`/usr/local/bin` gets `bin_t`.

**Manifest and socket are separate on purpose.** The daemon scans
`<data dir>/plugins` for manifests, unless `CC_PLUGINS_DIR` is set. Native packages use
`/var/lib/coolercontrol`. The official container image sets
`CC_DATA_DIR=/etc/coolercontrol`, so a containerised daemon scans
`/etc/coolercontrol/plugins`. The socket stays under `/var/lib/coolercontrol`, which has
to be bind-mounted into the container at the same path so that `address` resolves
there. A path-based Unix socket works across network namespaces, so the plugin's
`PrivateNetwork=yes` does not get in the way.

## systemd unit

[`dist/cc-gigabyte-gpu-rgb.service`](../dist/cc-gigabyte-gpu-rgb.service) runs the
service as root, because the I2C device nodes and the plugin directory belong to root,
but takes almost everything else away:

| Setting | Why |
|---|---|
| `CapabilityBoundingSet=` (empty), `NoNewPrivileges=yes` | root owns everything it needs, so no capabilities are required. The plugin directory must therefore be owned by root. |
| `DevicePolicy=closed`, `DeviceAllow=char-i2c rw` | only I2C character devices. `char-i2c` is resolved from `/proc/devices` when the service starts, so `i2c-dev` must be loaded first: `Wants=`/`After=systemd-modules-load.service` and the modules-load file. |
| no `PrivateDevices=` | it would hide `/dev/i2c-*` |
| `ProtectSystem=strict`, `ReadWritePaths=` plugin directory, `StateDirectory=` | read-only system; writable socket directory and state directory only. `ReadWritePaths=` fails with `226/NAMESPACE` if the directory is missing. |
| `PrivateNetwork=yes`, `RestrictAddressFamilies=AF_UNIX` | no network; Unix sockets only |
| `ProtectHome`, `PrivateTmp`, `ProtectKernel*`, `ProtectControlGroups`, `ProtectClock`, `ProtectHostname`, `ProtectProc=invisible` | general hardening; sysfs is only read |
| `SystemCallFilter=@system-service ~@privileged`, `MemoryDenyWriteExecute`, `LockPersonality`, `RestrictNamespaces`, `RestrictRealtime`, `RestrictSUIDSGID`, `SystemCallArchitectures=native` | general hardening; `ioctl` is part of `@system-service` |
| `UMask=0077` | socket and records private to root |
| `Before=coolercontrold.service coolercontrol.service` | the socket is up before the daemon connects. These are the names of the native unit and the usual container unit. |
| `TimeoutStopSec=15` | room for the bounded (5 s) auto-save flush |

`systemd-analyze security` rates it 1.2 ("OK"). The full root sandbox has been verified
on hardware.

## Auto-save

Goal: the card shows the user's look from power-on and after resume, without wearing
out the card's memory, whose write endurance is unknown.

### Behaviour

- After a successful `Lighting` apply, a save is scheduled for that device. It fires
  only after the setting has stayed unchanged for the delay (default 30 s,
  `--auto-save-delay`). Every apply restarts the wait.
- When it fires, it compares the setting now on the card with the record of what was
  last saved on that card. Equal: nothing is written. Different or unknown: it writes
  `AA`, then updates the record.
- The comparison uses the encoded packets (`protocol::encode` output), so equivalent
  requests count as the same.
- The record is updated only after the save write succeeded. A failed save is logged,
  the record is left alone, and it is not retried; the next apply schedules again. If
  writing the record fails after a successful save, that is logged and the service
  carries on.
- A failed apply (the card may hold half a setting) cancels any pending save.
- At shutdown a pending save is flushed with the same compare-then-write, bounded to
  5 s.
- `--no-auto-save` turns it off. Without a state directory (`--state-dir`, else
  `$STATE_DIRECTORY` from `StateDirectory=`) it is off, with a warning. A missing
  directory is created; if that fails, auto-save is off.

### Implementation

- Each device has one mutex over the bus, a generation counter, the packets of the
  last successful apply, and a "pending" flag (`DeviceInner` in
  [`src/service.rs`](../src/service.rs)). Applies and saves both take it, so a save
  never interleaves with an apply and always saves exactly what the last apply wrote.
- Every apply bumps the generation and spawns a timer task for it
  (`AutoSave::schedule` in [`src/autosave.rs`](../src/autosave.rs)). When a timer fires
  and its generation is no longer current, it does nothing (`Superseded`). Only the
  last apply's timer acts, which is a debounce with no cancellation.
- `GpuDevice::try_save` returns `Superseded`, `NothingPending`, `Unchanged`, `Saved`
  or `Failed`.
- The record is read from disk at every attempt, not cached, so changes the CLI makes
  are picked up.

### Record file

One file per card in the state directory:

```
<pci address>_<vendor>-<device>-<subsys vendor>-<subsys device>.saved
e.g. 0000:04:00.0_1002-747e-1458-2414.saved
```

The key includes the PCI IDs, so a different card in the same slot starts as "unknown".
The PCI address is sanitised to hex digits, `:` and `.`. Contents:

```
cc-gigabyte-gpu-rgb saved-state v1
88 01 02 63 00 01 00 00
b0 01 00 ff 00 00 ff 00
...
```

The packets are the encoded setting, one per line in hex. Writes are atomic: temp file,
fsync, rename, then fsync of the directory. A missing, unreadable or unparsable record
means "unknown": that leads to one save, never a crash.

### CLI interplay

The record is a belief about the card's memory, which cannot be read back:

| Command | Effect on the record |
|---|---|
| `set ... --save` | records the setting it saved. If recording fails, the record is deleted. |
| `save` | always writes `AA`, then deletes the record (it does not know what the card shows) |
| `set` without `--save` | none (the card's memory is unchanged) |

Both default to `/var/lib/cc-gigabyte-gpu-rgb` (`--state-dir` overrides). A missing
directory means nothing to update.

### Known limits

- Something else rewriting the card's memory (another RGB tool, vendor software, a
  firmware flash, a card reset) makes the record wrong while the CoolerControl setting
  is unchanged. The service will not save again until the setting changes. Fix: run
  `save` once.
- The service only knows its own applies. If `set` (or another tool) changes the card
  while a service save is pending, the save stores what the card shows but records the
  service's setting. Similarly, `set --save` during a pending service save costs one
  extra write and leaves the service's setting in the record. The next change or a
  manual `save` corrects both. There is no lock between the CLI and the service.
- The tokio timer uses the monotonic clock, which does not advance during suspend.
  After resume, CoolerControl's re-apply restarts the timer anyway. A change made less
  than the delay before a power cut is not saved.
- The first time a setting is seen, after a fresh install or after a manual `save`,
  costs one write even if the card already holds it.
- Auto-save has not yet run on hardware (unit-tested with a mock bus).

## Adding a card

See [adding-a-card.md](adding-a-card.md).
