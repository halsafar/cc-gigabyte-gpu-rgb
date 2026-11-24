# RGB Fusion 2 GPU lighting protocol

How the plugin talks to the lighting controller of a Gigabyte RGB Fusion 2 GPU, and
what of it has been checked on real hardware. The encoder is a pure function,
`protocol::encode` in [`src/protocol.rs`](../src/protocol.rs), with unit tests for the
exact bytes of every mode.

## Transport

- Every write is one plain 8-byte I2C write: the `I2C_RDWR` ioctl with a single
  message. These are not SMBus block writes.
- The only read is the 4-byte read in the probe.
- **Bus:** on the RX 7700 XT GAMING OC the controller sits on the adapter whose sysfs
  `name` starts with `AMDGPU DM i2c OEM bus`. The bus number changes between boots, so
  the plugin finds the bus by name and by the PCI IDs of the GPU it belongs to (see
  [design.md](design.md#bus-discovery)).
- **Address:** `0x62` on the RX 7700 XT GAMING OC. Other cards may use other addresses;
  see [adding-a-card.md](adding-a-card.md).

## Probe

Sent once at startup, and before every `probe`, `set` and `save` command.

1. Write `AB 00 00 00 00 00 00 00`.
2. In a separate transfer, read 4 bytes. This is a plain read after the write, not a
   combined write-then-read with a repeated start. The card answered this two-transfer
   form.
3. The reply is accepted if byte 0 is `AB` and bytes 1-3 are not all `AB`. A reply of
   `AB AB AB AB` comes from a bus that echoes the written byte back (DDC/EDID-style
   devices can do this), not from a controller. Any other first byte is rejected.

The RX 7700 XT GAMING OC answers `AB 10 52 03`. The last three bytes look like version
information; their meaning is not known.

## Applying a setting

For each zone `z` = 0, 1, 2, 3, 4, in that order:

1. **Mode packet:** `88 <mode> <speed> <brightness> <flag> <z+1> 00 00`
2. **Colour packet** (not sent for colour cycle, which picks its own colours):

   | Zone | Packet |
   |---|---|
   | 0 or 1 | `B0 <mode> R G B R G B` |
   | 2 | `B1 <mode> R G B 00 00 00` |
   | 3 or 4 | `40 R G B <z+1> 00 00 00` |

Notes:

- **Zones 0 and 1 share one colour register.** `B0` holds the colour of zone 0
  (first three colour bytes) and of zone 1 (last three) together. The plugin writes it
  after each of the two mode packets, so it goes out twice and every zone's mode packet
  is followed by its colour packet. All zones get the same colour, so both halves are
  the same.
- **All five zones are written every time.** Which of the five zones a card physically
  has is not known. On the RX 7700 XT GAMING OC writing all five works and causes no
  problems.
- **Brightness** is always `63`, the maximum. CoolerControl's lighting settings have no
  brightness value.
- **Flag** (fifth byte): `08` for gradient, `00` for every other mode. What else the
  byte controls is not known; gradient with `08` works on the RX 7700 XT GAMING OC.
- **Direction:** the protocol has none.

### Mode bytes

| Mode | Byte | Colour packet | Speed |
|---|---|---|---|
| `static` | `01` | yes | ignored, `02` sent |
| `breathing` | `02` | yes | yes |
| `color_cycle` | `03` | no | yes |
| `flashing` | `04` | yes | yes |
| `gradient` | `05` | yes (flag `08`) | yes |
| `double_flash` | `08` | yes | yes |
| `off` | `01` (static) with colour `00 00 00` | yes | ignored, `02` sent |

### Speed bytes

| Name | Byte |
|---|---|
| `slowest` | `00` |
| `slower` | `01` |
| `normal` | `02` |
| `faster` | `03` |
| `fastest` | `05` |

`04` is skipped: the five names map to `00 01 02 03 05`. Modes without speed control
always send `02`. Equivalent requests, such as `static` with different speeds, therefore
encode to identical packets.

### Example: static red on all zones by hand

With `i2ctransfer` from i2c-tools, as root:

```sh
N=$(grep -l '^AMDGPU DM i2c OEM bus' /sys/bus/i2c/devices/i2c-*/name | sed 's|.*/i2c-\([0-9]*\)/name|\1|')
i2ctransfer -y $N w8@0x62 0xab 0 0 0 0 0 0 0      # probe: write
i2ctransfer -y $N r4@0x62                           # probe: read, expect 0xab ...
for z in 0 1 2 3 4; do
  i2ctransfer -y $N w8@0x62 0x88 0x01 0x02 0x63 0x00 $((z+1)) 0 0
  case $z in
    0|1) i2ctransfer -y $N w8@0x62 0xb0 0x01 0xff 0 0 0xff 0 0 ;;
    2)   i2ctransfer -y $N w8@0x62 0xb1 0x01 0xff 0 0 0 0 0 ;;
    *)   i2ctransfer -y $N w8@0x62 0x40 0xff 0 0 $((z+1)) 0 0 0 ;;
  esac
done
```

If several GPUs have an OEM bus, `N` holds several numbers; pick the right one. The unit
test `static_red_matches_hand_test` checks that the encoder produces these bytes.

## Save

`AA 00 00 00 00 00 00 00` stores the lighting the card is showing in the card's own
memory. The card shows the stored setting from power-on and straight after resume.
Nothing needs to follow it.

The plugin sends it from the debounced, change-only auto-save
([design.md](design.md#auto-save)) and from the `save` and `set --save` commands. How many
writes the card's memory can take is unknown.

## Modes left out

| Mode | Byte | Why |
|---|---|---|
| Wave | `07` | On the RX 7700 XT GAMING OC it shows a solid colour with no animation, both with mode packets alone and with colour packets added after each mode packet. |
| Colour shift | `06` | Needs the multi-colour bank packets (`B0+z*4` to `B3+z*4`), not implemented. |
| Tricolor | `0B` | Same as colour shift. |

## Verified on hardware

On a Gigabyte Radeon RX 7700 XT GAMING OC:

| What | Status |
|---|---|
| Probe, reply `ab 10 52 03` | verified |
| Every exposed mode at `normal` speed, via the `set` command | verified |
| Every exposed mode from the CoolerControl UI (5.1.2, in a container) | verified |
| `off` as static black turns the card dark | verified |
| Re-apply after suspend/resume (CoolerControl re-sends the setting) | verified |
| Save packet sent by hand: card came back from suspend in the saved look | verified |
| Wave (`07`) | not a wave on this card (solid colour); left out |
| Speeds other than `normal` from the command line | not tested |
| Auto-save, and the `save` / `set --save` commands | not yet run on hardware |
| Behaviour after a reboot | not tested |
| Which of the five zones the card physically has | not known |
