// SPDX-License-Identifier: GPL-3.0-or-later
//
// Gigabyte RGB Fusion 2 GPU lighting protocol.
//
// See docs/protocol.md for the full description and what has been verified on
// hardware.
//
// Everything in this module is pure: it turns a lighting request into the ordered
// list of 8-byte packets to write. No I/O happens here.

use std::fmt;

/// One I2C write. Every write the card understands is exactly 8 bytes.
pub type Packet = [u8; 8];

/// Number of zones the protocol addresses (zone numbers 1-5 in the packets). All five
/// are written every time, because which zones a given card physically has is not
/// known. On the RX 7700 XT GAMING OC writing all five works and causes no problems.
pub const NUM_ZONES: u8 = 5;

/// Brightness value used for every mode (`0x63` is the card's maximum).
/// CoolerControl's `LightingSetting` has no brightness field.
pub const BRIGHTNESS_MAX: u8 = 0x63;

const REG_COLOR: u8 = 0x40;
const REG_MODE: u8 = 0x88;
const REG_COLOR_LEFT_MID: u8 = 0xB0;
const REG_COLOR_RIGHT: u8 = 0xB1;

/// Probe command. The card answers a following 4-byte read with `AB <version bytes>`.
pub const PROBE_PACKET: Packet = [0xAB, 0, 0, 0, 0, 0, 0, 0];
pub const PROBE_REPLY_LEN: usize = 4;

/// Save command. Stores the lighting currently shown in the card's own memory, so the
/// card shows it from power-on and after resume. Only the manual `save` command sends
/// it; the gRPC service never does, because the memory's write endurance is unknown.
pub const SAVE_PACKET: Packet = [0xAA, 0, 0, 0, 0, 0, 0, 0];

/// The packets that save the current lighting to the card: just [`SAVE_PACKET`].
pub fn encode_save() -> Vec<Packet> {
    vec![SAVE_PACKET]
}

/// Lighting modes exposed to CoolerControl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Static,
    Breathing,
    Flashing,
    DoubleFlash,
    Gradient,
    ColorCycle,
    Off,
}

impl Mode {
    pub const ALL: [Mode; 7] = [
        Mode::Static,
        Mode::Breathing,
        Mode::Flashing,
        Mode::DoubleFlash,
        Mode::Gradient,
        Mode::ColorCycle,
        Mode::Off,
    ];

    /// The mode id used on the gRPC API and the CLI.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Static => "static",
            Mode::Breathing => "breathing",
            Mode::Flashing => "flashing",
            Mode::DoubleFlash => "double_flash",
            Mode::Gradient => "gradient",
            Mode::ColorCycle => "color_cycle",
            Mode::Off => "off",
        }
    }

    /// Human readable name shown in the CoolerControl UI.
    pub fn frontend_name(self) -> &'static str {
        match self {
            Mode::Static => "Static",
            Mode::Breathing => "Breathing",
            Mode::Flashing => "Flashing",
            Mode::DoubleFlash => "Double Flash",
            Mode::Gradient => "Gradient",
            Mode::ColorCycle => "Color Cycle",
            Mode::Off => "Off",
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }

    /// Mode byte sent to the card.
    pub fn card_mode(self) -> u8 {
        match self {
            Mode::Static | Mode::Off => 0x01,
            Mode::Breathing => 0x02,
            Mode::ColorCycle => 0x03,
            Mode::Flashing => 0x04,
            Mode::Gradient => 0x05,
            Mode::DoubleFlash => 0x08,
        }
    }

    /// Number of colours the user supplies (both min and max).
    pub fn colors(self) -> u32 {
        match self {
            Mode::Static
            | Mode::Breathing
            | Mode::Flashing
            | Mode::DoubleFlash
            | Mode::Gradient => 1,
            Mode::ColorCycle | Mode::Off => 0,
        }
    }

    pub fn speed_enabled(self) -> bool {
        !matches!(self, Mode::Static | Mode::Off)
    }

    /// The fifth byte of the mode packet: `0x08` for gradient, `0x00` for every other
    /// mode. What else the byte controls is not known; gradient with `0x08` has been
    /// verified on the RX 7700 XT GAMING OC.
    fn flag(self) -> u8 {
        match self {
            Mode::Gradient => 0x08,
            _ => 0x00,
        }
    }

    /// Whether a colour packet follows the mode packet for each zone.
    /// Colour cycle picks its own colours, so it sends only the mode packet.
    /// `off` is static black, so it does send a colour packet.
    fn sends_color(self) -> bool {
        !matches!(self, Mode::ColorCycle)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Animation speeds, slowest first, as listed to CoolerControl.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Speed {
    Slowest,
    Slower,
    #[default]
    Normal,
    Faster,
    Fastest,
}

impl Speed {
    pub const ALL: [Speed; 5] = [
        Speed::Slowest,
        Speed::Slower,
        Speed::Normal,
        Speed::Faster,
        Speed::Fastest,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Speed::Slowest => "slowest",
            Speed::Slower => "slower",
            Speed::Normal => "normal",
            Speed::Faster => "faster",
            Speed::Fastest => "fastest",
        }
    }

    pub fn from_name(name: &str) -> Option<Speed> {
        Speed::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Speed byte sent to the card: `0x00` slowest .. `0x05` fastest.
    pub fn byte(self) -> u8 {
        match self {
            Speed::Slowest => 0x00,
            Speed::Slower => 0x01,
            Speed::Normal => 0x02,
            Speed::Faster => 0x03,
            Speed::Fastest => 0x05,
        }
    }
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Parses `RRGGBB` or `#RRGGBB`.
    pub fn parse_hex(s: &str) -> Option<Rgb> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        Some(Rgb::new(byte(0)?, byte(2)?, byte(4)?))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The mode needs a colour and none was given.
    MissingColor(Mode),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::MissingColor(m) => write!(f, "mode '{m}' requires one colour"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encodes a lighting setting into the ordered list of packets to write.
///
/// * `speed` is ignored by modes without speed control; they always send `normal`
///   (`0x02`). The card ignores the speed for these modes; `static` and `off` have
///   been verified with this value.
/// * `color` is required for modes with one colour, ignored otherwise.
///   `off` always uses black.
///
/// For each zone `z` in `0..5` the output is the mode packet followed (except for
/// colour-less modes) by the colour packet:
///
/// * mode: `88 <mode> <speed> 63 <flag> <z+1> 00 00`
/// * colour, z = 0 or 1: `B0 <mode> R G B R G B`. One register holds the colours of
///   zones 0 and 1 together, so it is written after each of their mode packets, i.e.
///   twice. Every zone's mode packet is thus followed by its colour packet.
/// * colour, z = 2: `B1 <mode> R G B 00 00 00`
/// * colour, z = 3 or 4: `40 R G B <z+1> 00 00 00`
pub fn encode(mode: Mode, speed: Speed, color: Option<Rgb>) -> Result<Vec<Packet>, EncodeError> {
    let color = match mode {
        Mode::Off => Rgb::BLACK,
        _ if mode.colors() == 0 => Rgb::BLACK,
        _ => color.ok_or(EncodeError::MissingColor(mode))?,
    };
    let speed = if mode.speed_enabled() {
        speed
    } else {
        Speed::Normal
    };
    let m = mode.card_mode();
    let Rgb { r, g, b } = color;

    let mut packets = Vec::with_capacity(usize::from(NUM_ZONES) * 2);
    for z in 0..NUM_ZONES {
        packets.push([
            REG_MODE,
            m,
            speed.byte(),
            BRIGHTNESS_MAX,
            mode.flag(),
            z + 1,
            0x00,
            0x00,
        ]);
        if !mode.sends_color() {
            continue;
        }
        packets.push(match z {
            0 | 1 => [REG_COLOR_LEFT_MID, m, r, g, b, r, g, b],
            2 => [REG_COLOR_RIGHT, m, r, g, b, 0, 0, 0],
            _ => [REG_COLOR, r, g, b, z + 1, 0, 0, 0],
        });
    }
    Ok(packets)
}

/// Result of checking the 4-byte probe reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// `AB` followed by version bytes: a Fusion 2 GPU controller.
    Valid,
    /// First byte is not `AB`.
    WrongReply,
    /// All four bytes are `AB`: a bus that echoes the write back, not a controller.
    Echo,
}

pub fn check_probe_reply(reply: &[u8; PROBE_REPLY_LEN]) -> ProbeVerdict {
    if reply[0] != 0xAB {
        ProbeVerdict::WrongReply
    } else if reply[1..].iter().all(|&b| b == 0xAB) {
        ProbeVerdict::Echo
    } else {
        ProbeVerdict::Valid
    }
}

/// Formats bytes as space separated hex, e.g. `88 01 02 63`.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgb = Rgb::new(0xff, 0x00, 0x00);
    const C: Rgb = Rgb::new(0x12, 0x34, 0x56);

    /// The packet list for a one-colour mode, written out by hand from the protocol notes.
    fn expected_with_color(m: u8, speed: u8, flag: u8, c: Rgb) -> Vec<Packet> {
        let Rgb { r, g, b } = c;
        vec![
            [0x88, m, speed, 0x63, flag, 1, 0, 0],
            [0xB0, m, r, g, b, r, g, b],
            [0x88, m, speed, 0x63, flag, 2, 0, 0],
            [0xB0, m, r, g, b, r, g, b],
            [0x88, m, speed, 0x63, flag, 3, 0, 0],
            [0xB1, m, r, g, b, 0, 0, 0],
            [0x88, m, speed, 0x63, flag, 4, 0, 0],
            [0x40, r, g, b, 4, 0, 0, 0],
            [0x88, m, speed, 0x63, flag, 5, 0, 0],
            [0x40, r, g, b, 5, 0, 0, 0],
        ]
    }

    fn expected_mode_only(m: u8, speed: u8) -> Vec<Packet> {
        (1..=5)
            .map(|z| [0x88, m, speed, 0x63, 0, z, 0, 0])
            .collect()
    }

    #[test]
    fn static_red_matches_hand_test() {
        // Same bytes as the manual i2ctransfer test in docs/protocol.md.
        let p = encode(Mode::Static, Speed::Normal, Some(RED)).unwrap();
        assert_eq!(p, expected_with_color(0x01, 0x02, 0x00, RED));
    }

    #[test]
    fn static_ignores_requested_speed() {
        let p = encode(Mode::Static, Speed::Fastest, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x01, 0x02, 0x00, C));
    }

    #[test]
    fn breathing() {
        let p = encode(Mode::Breathing, Speed::Slower, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x02, 0x01, 0x00, C));
    }

    #[test]
    fn flashing() {
        let p = encode(Mode::Flashing, Speed::Faster, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x04, 0x03, 0x00, C));
    }

    #[test]
    fn double_flash() {
        let p = encode(Mode::DoubleFlash, Speed::Fastest, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x08, 0x05, 0x00, C));
    }

    #[test]
    fn gradient_sets_flag() {
        let p = encode(Mode::Gradient, Speed::Slowest, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x05, 0x00, 0x08, C));
    }

    #[test]
    fn color_cycle_has_no_color_packets() {
        let p = encode(Mode::ColorCycle, Speed::Normal, None).unwrap();
        assert_eq!(p, expected_mode_only(0x03, 0x02));
        // A colour sent anyway is ignored.
        assert_eq!(
            encode(Mode::ColorCycle, Speed::Normal, Some(C)).unwrap(),
            expected_mode_only(0x03, 0x02)
        );
    }

    #[test]
    fn off_is_static_black() {
        let p = encode(Mode::Off, Speed::Fastest, Some(C)).unwrap();
        assert_eq!(p, expected_with_color(0x01, 0x02, 0x00, Rgb::BLACK));
        assert_eq!(p, encode(Mode::Off, Speed::Normal, None).unwrap());
    }

    #[test]
    fn zones_0_and_1_both_send_shared_b0_packet() {
        let p = encode(Mode::Static, Speed::Normal, Some(C)).unwrap();
        let b0: Vec<usize> = p
            .iter()
            .enumerate()
            .filter(|(_, pkt)| pkt[0] == 0xB0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(b0, vec![1, 3]);
        assert_eq!(p[1], p[3]);
        assert_eq!(p[0][5], 1);
        assert_eq!(p[2][5], 2);
    }

    #[test]
    fn missing_color_is_an_error() {
        for m in Mode::ALL.into_iter().filter(|m| m.colors() == 1) {
            assert_eq!(
                encode(m, Speed::Normal, None),
                Err(EncodeError::MissingColor(m))
            );
        }
    }

    #[test]
    fn every_mode_writes_every_zone_in_order() {
        for m in Mode::ALL {
            let p = encode(m, Speed::Normal, Some(C)).unwrap();
            let zones: Vec<u8> = p.iter().filter(|x| x[0] == 0x88).map(|x| x[5]).collect();
            assert_eq!(zones, vec![1, 2, 3, 4, 5], "{m}");
            assert!(
                p.iter().all(|x| x[0] != 0xAA),
                "{m} must never save to flash"
            );
            assert!(p.iter().filter(|x| x[0] == 0x88).all(|x| x[3] == 0x63));
        }
    }

    #[test]
    fn save_is_one_aa_packet() {
        assert_eq!(SAVE_PACKET, [0xAA, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(encode_save(), vec![[0xAA, 0, 0, 0, 0, 0, 0, 0]]);
    }

    #[test]
    fn speed_mapping() {
        let bytes: Vec<u8> = Speed::ALL.iter().map(|s| s.byte()).collect();
        assert_eq!(bytes, vec![0x00, 0x01, 0x02, 0x03, 0x05]);
        let names: Vec<&str> = Speed::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            vec!["slowest", "slower", "normal", "faster", "fastest"]
        );
        for s in Speed::ALL {
            assert_eq!(Speed::from_name(s.name()), Some(s));
        }
        assert_eq!(Speed::from_name("ludicrous"), None);
        assert_eq!(Speed::default(), Speed::Normal);
    }

    #[test]
    fn mode_table() {
        let rows: Vec<(&str, u8, u32, bool)> = Mode::ALL
            .iter()
            .map(|m| (m.name(), m.card_mode(), m.colors(), m.speed_enabled()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("static", 0x01, 1, false),
                ("breathing", 0x02, 1, true),
                ("flashing", 0x04, 1, true),
                ("double_flash", 0x08, 1, true),
                ("gradient", 0x05, 1, true),
                ("color_cycle", 0x03, 0, true),
                ("off", 0x01, 0, false),
            ]
        );
        for m in Mode::ALL {
            assert_eq!(Mode::from_name(m.name()), Some(m));
        }
        assert_eq!(Mode::from_name("Static"), None);
        // Removed: mode 0x07 shows a solid colour on the RX 7700 XT GAMING OC.
        assert_eq!(Mode::from_name("wave"), None);
    }

    #[test]
    fn probe_reply() {
        assert_eq!(
            check_probe_reply(&[0xAB, 0x10, 0x52, 0x07]),
            ProbeVerdict::Valid
        );
        assert_eq!(
            check_probe_reply(&[0xAB, 0xAB, 0xAB, 0x00]),
            ProbeVerdict::Valid
        );
        assert_eq!(
            check_probe_reply(&[0xAB, 0xAB, 0xAB, 0xAB]),
            ProbeVerdict::Echo
        );
        assert_eq!(
            check_probe_reply(&[0x00, 0x00, 0x00, 0x00]),
            ProbeVerdict::WrongReply
        );
        assert_eq!(
            check_probe_reply(&[0xFF, 0xFF, 0xFF, 0xFF]),
            ProbeVerdict::WrongReply
        );
    }

    #[test]
    fn parse_hex_colors() {
        assert_eq!(Rgb::parse_hex("ff0000"), Some(RED));
        assert_eq!(Rgb::parse_hex("#123456"), Some(C));
        assert_eq!(Rgb::parse_hex("ABCDEF"), Some(Rgb::new(0xab, 0xcd, 0xef)));
        assert_eq!(Rgb::parse_hex("fff"), None);
        assert_eq!(Rgb::parse_hex("gg0000"), None);
        assert_eq!(Rgb::parse_hex("+f0000"), None);
        assert_eq!(Rgb::parse_hex("ff00000"), None);
        assert_eq!(C.to_string(), "123456");
    }

    #[test]
    fn hex_format() {
        assert_eq!(hex(&[0x88, 0x01, 0xff]), "88 01 ff");
    }
}
