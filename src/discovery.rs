// SPDX-License-Identifier: GPL-3.0-or-later
//
// Finds the I2C bus of a supported card through sysfs.
//
// I2C bus numbers are not stable across boots, so the bus is found by its adapter
// name and the PCI IDs of the GPU it belongs to:
//
//   <sysfs>/bus/i2c/devices/i2c-N  ->  <sysfs>/devices/pci.../0000:03:00.0/i2c-N
//
// The adapter's sysfs path is resolved and walked upwards to the first directory
// that has `vendor` and `subsystem_device` files: that is the PCI function.

use crate::cards::{CardKey, PciIds, SUPPORTED_CARDS, SupportedCard};
use log::debug;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_SYSFS_ROOT: &str = "/sys";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundBus {
    pub card: &'static SupportedCard,
    /// N in `/dev/i2c-N`.
    pub bus_number: u32,
    /// Adapter name from sysfs.
    pub bus_name: String,
    /// PCI address of the GPU, e.g. `0000:03:00.0`.
    pub pci_address: String,
    /// Resolved sysfs path of the adapter.
    pub sysfs_path: PathBuf,
}

impl FoundBus {
    pub fn dev_path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/i2c-{}", self.bus_number))
    }

    /// The card's identity; its PCI IDs are the ones matched against the table.
    pub fn key(&self) -> CardKey {
        CardKey {
            pci_address: self.pci_address.clone(),
            ids: self.card.ids(),
        }
    }
}

/// Lists every I2C adapter under `sysfs_root` that belongs to a card in `cards`,
/// sorted by bus number. An empty list means no supported card was found.
pub fn discover(sysfs_root: &Path, cards: &'static [SupportedCard]) -> io::Result<Vec<FoundBus>> {
    let devices_dir = sysfs_root.join("bus/i2c/devices");
    let root = fs::canonicalize(sysfs_root)?;
    let mut found = Vec::new();

    for entry in fs::read_dir(&devices_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(bus_number) = file_name
            .to_str()
            .and_then(|n| n.strip_prefix("i2c-"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            // Client devices such as `21-0050`, or anything unexpected.
            continue;
        };
        let link = entry.path();
        let Ok(bus_name) = fs::read_to_string(link.join("name")) else {
            continue;
        };
        let bus_name = bus_name.trim_end().to_string();

        let candidates: Vec<&'static SupportedCard> = cards
            .iter()
            .filter(|c| bus_name.starts_with(c.bus_name_prefix))
            .collect();
        if candidates.is_empty() {
            continue;
        }

        let Ok(sysfs_path) = fs::canonicalize(&link) else {
            debug!("i2c-{bus_number}: cannot resolve {}", link.display());
            continue;
        };
        let Some((pci_dir, ids)) = find_pci_parent(&sysfs_path, &root) else {
            debug!("i2c-{bus_number} ('{bus_name}'): no PCI parent found");
            continue;
        };
        let Some(card) = candidates.into_iter().find(|c| c.matches_ids(&ids)) else {
            debug!("i2c-{bus_number} ('{bus_name}'): PCI IDs {ids} not in the supported list");
            continue;
        };
        let pci_address = pci_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        debug!(
            "i2c-{bus_number} ('{bus_name}') belongs to {} at {pci_address}",
            card.name
        );
        found.push(FoundBus {
            card,
            bus_number,
            bus_name,
            pci_address,
            sysfs_path,
        });
    }
    found.sort_by_key(|f| f.bus_number);
    Ok(found)
}

/// Same as [`discover`] with the built-in card table.
pub fn discover_supported(sysfs_root: &Path) -> io::Result<Vec<FoundBus>> {
    discover(sysfs_root, SUPPORTED_CARDS)
}

/// Walks up from `start` (exclusive) to `root` looking for a PCI function directory.
fn find_pci_parent(start: &Path, root: &Path) -> Option<(PathBuf, PciIds)> {
    let mut dir = start.parent();
    while let Some(d) = dir {
        if !d.starts_with(root) || d == root {
            return None;
        }
        if d.join("vendor").is_file() && d.join("subsystem_device").is_file() {
            return read_ids(d).map(|ids| (d.to_path_buf(), ids));
        }
        dir = d.parent();
    }
    None
}

fn read_ids(dir: &Path) -> Option<PciIds> {
    let read = |name: &str| -> Option<u16> {
        let s = fs::read_to_string(dir.join(name)).ok()?;
        let s = s.trim();
        u16::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).ok()
    };
    Some(PciIds {
        vendor: read("vendor")?,
        device: read("device")?,
        subsystem_vendor: read("subsystem_vendor")?,
        subsystem_device: read("subsystem_device")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const GPU_PCI: &str = "devices/pci0000:00/0000:00:06.0/0000:02:00.0/0000:03:00.0/0000:04:00.0";

    struct FakeSysfs {
        dir: tempfile::TempDir,
    }

    impl FakeSysfs {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("bus/i2c/devices")).unwrap();
            FakeSysfs { dir }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn pci(&self, rel: &str, ids: (u16, u16, u16, u16)) {
            let d = self.root().join(rel);
            fs::create_dir_all(&d).unwrap();
            for (name, v) in [
                ("vendor", ids.0),
                ("device", ids.1),
                ("subsystem_vendor", ids.2),
                ("subsystem_device", ids.3),
            ] {
                fs::write(d.join(name), format!("0x{v:04x}\n")).unwrap();
            }
        }

        /// Creates `<rel_parent>/i2c-N` with a name and the bus/i2c/devices symlink.
        fn adapter(&self, rel_parent: &str, n: u32, name: &str) {
            let d = self.root().join(rel_parent).join(format!("i2c-{n}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("name"), format!("{name}\n")).unwrap();
            let target = PathBuf::from("../../..")
                .join(rel_parent)
                .join(format!("i2c-{n}"));
            symlink(target, self.root().join(format!("bus/i2c/devices/i2c-{n}"))).unwrap();
        }
    }

    const IDS: (u16, u16, u16, u16) = (0x1002, 0x747e, 0x1458, 0x2414);

    #[test]
    fn finds_oem_bus_by_name_and_ids() {
        let fs_ = FakeSysfs::new();
        fs_.pci(GPU_PCI, IDS);
        // Other adapters on the same GPU, numbered around the OEM bus.
        fs_.adapter(GPU_PCI, 19, "AMDGPU DM i2c hw bus 1");
        fs_.adapter(GPU_PCI, 20, "AMDGPU DM aux hw bus 0");
        fs_.adapter(GPU_PCI, 21, "AMDGPU DM i2c OEM bus");
        // A client device entry, which must be ignored.
        fs::create_dir_all(fs_.root().join("bus/i2c/devices/21-0050")).unwrap();

        let found = discover_supported(fs_.root()).unwrap();
        assert_eq!(found.len(), 1);
        let f = &found[0];
        assert_eq!(f.bus_number, 21);
        assert_eq!(f.bus_name, "AMDGPU DM i2c OEM bus");
        assert_eq!(f.pci_address, "0000:04:00.0");
        assert_eq!(f.card.name, "Gigabyte Radeon RX 7700 XT GAMING OC");
        assert_eq!(f.card.address, 0x62);
        assert_eq!(f.dev_path(), PathBuf::from("/dev/i2c-21"));
        assert!(f.sysfs_path.ends_with("0000:04:00.0/i2c-21"));
    }

    #[test]
    fn walks_up_through_intermediate_dirs() {
        let fs_ = FakeSysfs::new();
        fs_.pci(GPU_PCI, IDS);
        fs_.adapter(&format!("{GPU_PCI}/some-child"), 7, "AMDGPU DM i2c OEM bus");
        let found = discover_supported(fs_.root()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pci_address, "0000:04:00.0");
    }

    #[test]
    fn rejects_other_subsystem_ids() {
        let fs_ = FakeSysfs::new();
        // Same GPU from another board partner.
        fs_.pci(GPU_PCI, (0x1002, 0x747e, 0x1da2, 0x2414));
        fs_.adapter(GPU_PCI, 21, "AMDGPU DM i2c OEM bus");
        assert!(discover_supported(fs_.root()).unwrap().is_empty());
    }

    #[test]
    fn rejects_other_bus_names() {
        let fs_ = FakeSysfs::new();
        fs_.pci(GPU_PCI, IDS);
        fs_.adapter(GPU_PCI, 3, "AMDGPU DM i2c hw bus 0");
        fs_.adapter(GPU_PCI, 4, "AMDGPU SMU 0");
        assert!(discover_supported(fs_.root()).unwrap().is_empty());
    }

    #[test]
    fn ignores_adapters_without_pci_parent() {
        let fs_ = FakeSysfs::new();
        // Name matches but no PCI device above it.
        fs_.adapter("devices/platform/foo", 2, "AMDGPU DM i2c OEM bus");
        assert!(discover_supported(fs_.root()).unwrap().is_empty());
    }

    #[test]
    fn does_not_walk_above_sysfs_root() {
        // Put vendor/subsystem files *above* the fake sysfs root; they must not be used.
        let outer = tempfile::tempdir().unwrap();
        for (name, v) in [
            ("vendor", "0x1002"),
            ("device", "0x747e"),
            ("subsystem_vendor", "0x1458"),
            ("subsystem_device", "0x2414"),
        ] {
            fs::write(outer.path().join(name), v).unwrap();
        }
        let root = outer.path().join("sys");
        fs::create_dir_all(root.join("bus/i2c/devices")).unwrap();
        let d = root.join("devices/virtual/i2c-1");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("name"), "AMDGPU DM i2c OEM bus\n").unwrap();
        symlink(
            "../../../devices/virtual/i2c-1",
            root.join("bus/i2c/devices/i2c-1"),
        )
        .unwrap();
        assert!(discover_supported(&root).unwrap().is_empty());
    }

    #[test]
    fn multiple_cards_sorted() {
        let fs_ = FakeSysfs::new();
        let other = "devices/pci0000:00/0000:00:01.0/0000:0a:00.0";
        fs_.pci(GPU_PCI, IDS);
        fs_.pci(other, IDS);
        fs_.adapter(other, 30, "AMDGPU DM i2c OEM bus");
        fs_.adapter(GPU_PCI, 21, "AMDGPU DM i2c OEM bus");
        let found = discover_supported(fs_.root()).unwrap();
        let got: Vec<(u32, &str)> = found
            .iter()
            .map(|f| (f.bus_number, f.pci_address.as_str()))
            .collect();
        assert_eq!(got, vec![(21, "0000:04:00.0"), (30, "0000:0a:00.0")]);
    }

    #[test]
    fn missing_sysfs_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_supported(&dir.path().join("nope")).is_err());
    }
}
