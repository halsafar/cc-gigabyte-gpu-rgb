# cc-gigabyte-gpu-rgb

A [CoolerControl](https://coolercontrol.org) plugin that controls the lighting of
Gigabyte graphics cards with an RGB Fusion 2 GPU controller, over I2C. CoolerControl
already sees the card's fans and temperatures; this plugin adds the LEDs as a lighting
device with one channel, `GPU`. It talks to one I2C address on one bus and does not
scan other devices. It does no polling: it only writes when you change a setting. It
saves your look to the card's memory, so the card also shows it at power-on and after
resume; this is debounced and happens only when the look actually changes. It does not
do per-LED or per-zone colours, brightness or direction.

## Supported cards

| Card | PCI ID | Subsystem |
|---|---|---|
| Gigabyte Radeon RX 7700 XT GAMING OC | `1002:747e` | `1458:2414` |

Check your card with `lspci -nn -d ::0300` (PCI ID) and
`lspci -nnv -s <address> | grep Subsystem`. Other Gigabyte cards may work once added:
see [docs/adding-a-card.md](docs/adding-a-card.md).

**Status** on the RX 7700 XT GAMING OC:

| Verified on hardware | Not yet run on hardware |
|---|---|
| probe (reply `ab 10 52 03`) | automatic saving |
| every mode at `normal` speed, via `set` and from the CoolerControl UI | the `save` and `set --save` commands |
| the systemd unit with its full root sandbox | speeds other than `normal` from the command line |
| CoolerControl 5.1.2 integration, daemon in a container | behaviour after a reboot |
| re-apply after suspend/resume | |
| the save packet, sent by hand | |

## Modes

| Mode | Colours | Speed |
|---|---|---|
| `static` | 1 | no |
| `breathing` | 1 | yes |
| `flashing` | 1 | yes |
| `double_flash` | 1 | yes |
| `gradient` | 1 | yes |
| `color_cycle` | 0 | yes |
| `off` | 0 | no |

Speeds: `slowest`, `slower`, `normal`, `faster`, `fastest`. All zones get the same
colour at full brightness. The card's wave, colour shift and tricolor modes are not
offered ([why](docs/protocol.md#modes-left-out)).

## Install

The plugin runs as its own systemd service on the host; CoolerControl connects to it
through a socket.

```sh
make build                                # as your user; builds in a Podman container
sudo make install                         # installs, enables and starts the service
sudo systemctl restart coolercontrold     # or your CoolerControl container's unit
```

If you run CoolerControl in a container, read the next section first. Building needs
only Podman and `make`; for other options see [docs/build.md](docs/build.md).

`make install` is idempotent. It installs:

| What | Where |
|---|---|
| binary | `/usr/local/bin/cc-gigabyte-gpu-rgb` |
| systemd unit | `/etc/systemd/system/cc-gigabyte-gpu-rgb.service` |
| `i2c-dev` autoload | `/etc/modules-load.d/cc-gigabyte-gpu-rgb.conf` |
| manifest | `/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb/manifest.toml` (see below) |
| socket | `/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb/gigabyte-gpu-rgb.sock` |
| save records | `/var/lib/cc-gigabyte-gpu-rgb/` |

To install by hand, copy the files from `dist/` to those places, create the socket
directory, and run `sudo systemctl daemon-reload && sudo systemctl enable --now
cc-gigabyte-gpu-rgb`. Keep the binary out of the plugins directory: on SELinux systems
systemd cannot execute files there (`status=203/EXEC`).

### CoolerControl in a container

The official CoolerControl container image looks for plugins in
`/etc/coolercontrol/plugins`, not `/var/lib/coolercontrol/plugins`. With the default
install the daemon never sees the plugin. Two requirements and one fix:

- `/var/lib/coolercontrol` must be bind-mounted into the container at the same path,
  because the socket lives there.
- Then either install the manifest where the container looks (this also needs
  `/etc/coolercontrol` mounted):

  ```sh
  sudo make install MANIFEST_PLUGINS_DIR=/etc/coolercontrol/plugins
  ```

  or keep the default install and set `CC_PLUGINS_DIR=/var/lib/coolercontrol/plugins`
  in the container's environment (`Environment=` in a Podman quadlet's `[Container]`
  section, or `-e` for `podman run`/`docker run`), then recreate the container.

Restart the container afterwards.

### Check it works

`journalctl -u cc-gigabyte-gpu-rgb -b` should show `Found Gigabyte Radeon RX 7700 XT
GAMING OC ... probe reply ab ...` and `Listening on ...`. After restarting CoolerControl,
its log lists the device under `Initialized Service Plugin Devices`, and the card
appears in the UI with a `GPU` lighting channel.

## Using it

Pick a mode, colour and speed for the card's `GPU` channel in CoolerControl, like any
other lighting device.

The binary also works on its own (as root), which is handy for testing:

```sh
sudo cc-gigabyte-gpu-rgb probe                              # find the card, print its reply
sudo cc-gigabyte-gpu-rgb set static --color ff0000          # apply a setting
sudo cc-gigabyte-gpu-rgb set breathing --color 0000ff --speed slower
sudo cc-gigabyte-gpu-rgb set static --color 00ff00 --save   # apply and save to the card
sudo cc-gigabyte-gpu-rgb save                               # save what the card shows now
```

Add `--debug` to log every I2C transfer. `cc-gigabyte-gpu-rgb --help` lists all options.

## Saving to the card

The card keeps one setting in its own memory and shows it from power-on and right
after resume, until CoolerControl takes over (from the factory that is a rainbow). The
plugin saves your CoolerControl setting there automatically:

- only after the setting has stayed unchanged for 30 seconds, so clicking through
  colours costs one save;
- only if it differs from what the plugin last saved on that card. CoolerControl
  re-sends the same setting at every start and resume, and that costs nothing.

How many writes the card's memory can take is unknown, which is why saving is this
careful. To change the delay or turn saving off, add `--auto-save-delay <seconds>` or
`--no-auto-save` to `ExecStart=` with `sudo systemctl edit --full cc-gigabyte-gpu-rgb`.
Don't run `save` or `set --save` from scripts or timers.

**One limit.** The card's memory cannot be read back, so the plugin only remembers what
it saved. If something else rewrites the card (other RGB software, the vendor's Windows
tool, a firmware flash or a card reset), the card shows that look at power-on even though
your CoolerControl setting is unchanged. Fix: run `sudo cc-gigabyte-gpu-rgb save` once,
or change the setting. Live control is never affected.

Details: [docs/design.md](docs/design.md#auto-save).

## Troubleshooting

| Symptom | Fix |
|---|---|
| Service running, but CoolerControl logs nothing about the plugin and shows `Initialized Service Plugin Devices: []` | The daemon isn't reading the manifest; usually CoolerControl runs in a container. See [CoolerControl in a container](#coolercontrol-in-a-container). |
| CoolerControl doesn't show the device | Restart CoolerControl after (re)installing the plugin. Check that `address` in the manifest resolves to the same path inside the container. |
| `No supported card found` | Load `i2c-dev` (`sudo modprobe i2c-dev`), check your card's IDs against the table, run `probe` with `--debug`. |
| `cannot open /dev/i2c-N: Permission denied` | Run as root. Under systemd, `i2c-dev` must be loaded before the service starts; restart the service after loading it by hand. |
| Probe fails, or the reply is `ab ab ab ab` / `00 00 00 00` | That bus or address has no compatible controller. Do not force writes. |
| Service fails with `status=226/NAMESPACE` | `/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb` is missing. Re-run `sudo make install`. |
| `cannot bind ...` / permission denied on the socket | The plugin directory must be owned by root (the service runs as root without capabilities). |
| Card shows another look at power-on or after resume | The setting changed less than 30 s before shutdown or suspend, or something else rewrote the card. Run `sudo cc-gigabyte-gpu-rgb save` once. Each save is logged as `saved the lighting setting to the card's memory`. |
| Some LEDs stay dark or colours look wrong | Please report your card and what lights. |

## Uninstall

```sh
sudo make uninstall          # add MANIFEST_PLUGINS_DIR=... if you used it to install
```

This removes the binary, the unit, the modules-load file, the plugin's own directories
and the save records, then you restart CoolerControl.

## Safety

The plugin only writes to a card that is in its table *and* answers the probe. Writing
to the wrong I2C device can confuse a monitor or other hardware, so don't add cards
without following [docs/adding-a-card.md](docs/adding-a-card.md). The service runs as
root without capabilities, with no network, a read-only filesystem apart from its own
directories, and access to I2C devices only ([details](docs/design.md#systemd-unit)).

## Documentation

- [docs/protocol.md](docs/protocol.md): the card's I2C protocol, and what was verified
  on hardware.
- [docs/design.md](docs/design.md): service behaviour, paths, systemd sandbox,
  auto-save design.
- [docs/adding-a-card.md](docs/adding-a-card.md): finding IDs, bus and address for
  another card.
- [docs/build.md](docs/build.md): building, testing, repository layout.

## Licence and credits

Copyright (C) the cc-gigabyte-gpu-rgb contributors. Licensed under the GNU General
Public License, version 3 or (at your option) any later version; see
[LICENSE](LICENSE).

The service skeleton, build script and the vendored `proto/` files are derived from the
CoolerControl [device service template](https://gitlab.com/coolercontrol/cc-plugins)
(GPL-3.0-or-later, `cc-plugins` commit `47edcf3a`). The `proto/` files are unmodified.

Not affiliated with Gigabyte, AMD or CoolerControl.
