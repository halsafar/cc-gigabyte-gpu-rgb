# Adding a card

The plugin only talks to cards listed in `SUPPORTED_CARDS` in
[`src/cards.rs`](../src/cards.rs). Each entry needs the card's four PCI IDs, a name, the
I2C adapter name prefix and the controller's I2C address. Only cards whose lighting
controller speaks the same RGB Fusion 2 GPU protocol ([protocol.md](protocol.md)) will
work.

**Caution.** Finding the address means sending I2C transfers to devices you have not
identified. A write to the wrong device can confuse a monitor (DDC/EDID), a voltage
regulator or other hardware on the bus. Only probe buses that belong to the GPU. Probe
one address at a time. Never send lighting packets to an address that did not answer the
probe. Do this at your own risk.

## 1. PCI IDs

```sh
lspci -nn -d ::0300                       # e.g. 03:00.0 VGA ... [1002:747e]
lspci -nnv -s 03:00.0 | grep Subsystem    # e.g. Subsystem: ... [1458:2414]
```

Or read them from sysfs. Use the full address, and note that on some cards the GPU
function sits behind internal bridges, so check that it is the VGA function:

```sh
d=/sys/bus/pci/devices/0000:03:00.0
cat $d/vendor $d/device $d/subsystem_vendor $d/subsystem_device
```

## 2. The I2C bus

Load `i2c-dev` (`sudo modprobe i2c-dev`). List the adapters that belong to the GPU, by
resolving each adapter's sysfs path:

```sh
for a in /sys/bus/i2c/devices/i2c-*; do
  printf '%s  %s  %s\n' "${a##*/}" "$(cat $a/name)" "$(readlink -f $a)"
done | grep 0000:03:00.0
```

On the RX 7700 XT GAMING OC the controller is on `AMDGPU DM i2c OEM bus`. Other AMD
cards probably use the same adapter. NVIDIA cards use different adapter names. The
plugin walks up from the adapter to the first directory with PCI ID files, so an
adapter nested under the GPU's PCI function is fine.

The bus number (`N` in `i2c-N`) changes between boots. Use the name prefix in the
table, not the number.

## 3. The controller address

A controller answers the probe: write `AB 00 00 00 00 00 00 00`, then in a separate
transfer read 4 bytes. The reply is `AB` followed by three version bytes (the RX 7700 XT
GAMING OC answers `AB 10 52 03`). `AB AB AB AB` is an echoing bus, not a controller.

With the plugin: add a trial entry with a candidate address, build, and run
`sudo cc-gigabyte-gpu-rgb --debug probe`.

With i2c-tools, for one candidate address `A` on bus `N`:

```sh
sudo i2ctransfer -y N w8@A 0xab 0 0 0 0 0 0 0
sudo i2ctransfer -y N r4@A
```

`0x62` is the address on the RX 7700 XT GAMING OC. Do not sweep the whole address
range with writes. `i2cdetect` can list which addresses acknowledge, but it also sends
transfers; prefer its read mode (`-r`) and keep to the GPU's bus.

## 4. Add the entry

```rust
SupportedCard {
    vendor: 0x1002,
    device: 0x747e,
    subsystem_vendor: 0x1458,
    subsystem_device: 0x2414,
    name: "Gigabyte Radeon RX 7700 XT GAMING OC",
    bus_name_prefix: "AMDGPU DM i2c OEM bus",
    address: 0x62,
},
```

Build ([build.md](build.md)), then check with `probe`, then
`set static --color ff0000`, then the other modes. Watch for modes that behave
differently on your card (wave, for example, is not a wave on the RX 7700 XT GAMING OC).

## 5. Send it upstream

Open a pull request with the card's name, the four IDs, the bus name, the address, the
probe reply, and which modes you tested.
