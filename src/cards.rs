// SPDX-License-Identifier: GPL-3.0-or-later
//
// Cards this plugin knows how to find.
//
// To add a card, append an entry with:
//
// * PCI IDs: `lspci -nn` and `lspci -nnv -s <address> | grep Subsystem`, or the
//   files /sys/bus/pci/devices/<address>/{vendor,device,subsystem_vendor,
//   subsystem_device}.
// * I2C adapter name prefix: `cat /sys/bus/i2c/devices/i2c-*/name` for the adapters
//   whose sysfs path lies under the card's PCI device (`readlink -f`).
// * Controller address: found by probing candidate addresses on that bus, with
//   `cc-gigabyte-gpu-rgb probe` after adding a trial entry, or with i2c-tools.
//   A controller answers `AB` followed by version bytes. Probing unknown I2C
//   addresses writes to whatever device is there (a monitor's DDC/EDID, a voltage
//   controller, ...): only probe buses that belong to the card, never write lighting
//   packets to an address that did not answer the probe, and do it at your own risk.
//
// See docs/adding-a-card.md for the full procedure.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupportedCard {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
    /// Name reported to CoolerControl.
    pub name: &'static str,
    /// The I2C adapter's `name` in sysfs must start with this.
    pub bus_name_prefix: &'static str,
    /// 7-bit I2C address of the lighting controller.
    pub address: u16,
}

const AMD: u16 = 0x1002;
const GIGABYTE: u16 = 0x1458;
const AMD_OEM_BUS: &str = "AMDGPU DM i2c OEM bus";

pub const SUPPORTED_CARDS: &[SupportedCard] = &[SupportedCard {
    vendor: AMD,
    device: 0x747e,
    subsystem_vendor: GIGABYTE,
    subsystem_device: 0x2414,
    name: "Gigabyte Radeon RX 7700 XT GAMING OC",
    bus_name_prefix: AMD_OEM_BUS,
    address: 0x62,
}];

impl SupportedCard {
    pub fn ids(&self) -> PciIds {
        PciIds {
            vendor: self.vendor,
            device: self.device,
            subsystem_vendor: self.subsystem_vendor,
            subsystem_device: self.subsystem_device,
        }
    }

    pub fn matches_ids(&self, ids: &PciIds) -> bool {
        self.vendor == ids.vendor
            && self.device == ids.device
            && self.subsystem_vendor == ids.subsystem_vendor
            && self.subsystem_device == ids.subsystem_device
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIds {
    pub vendor: u16,
    pub device: u16,
    pub subsystem_vendor: u16,
    pub subsystem_device: u16,
}

/// Identifies one physical card: where it sits and what it is. Used to key the
/// saved-state record, so a different card in the same slot starts as "unknown".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardKey {
    pub pci_address: String,
    pub ids: PciIds,
}

impl std::fmt::Display for PciIds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:04x}:{:04x} subsystem {:04x}:{:04x}",
            self.vendor, self.device, self.subsystem_vendor, self.subsystem_device
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sane() {
        assert!(!SUPPORTED_CARDS.is_empty());
        for c in SUPPORTED_CARDS {
            assert!(c.address < 0x80, "{} address must be 7-bit", c.name);
            assert!(!c.name.is_empty());
            assert!(!c.bus_name_prefix.is_empty());
        }
    }
}
