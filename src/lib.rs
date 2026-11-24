// SPDX-License-Identifier: GPL-3.0-or-later
//
// cc-gigabyte-gpu-rgb: CoolerControl device-service plugin for the lighting of
// Gigabyte RGB Fusion 2 GPUs over I2C.
//
// Copyright (C) the cc-gigabyte-gpu-rgb contributors.
// Service skeleton derived from the CoolerControl device-service template
// (GPL-3.0-or-later).
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, either version 3 of the License, or (at your option) any later
// version. See the LICENSE file.

pub mod autosave;
pub mod bus;
pub mod cards;
pub mod discovery;
pub mod protocol;
pub mod server;
pub mod service;

pub const SERVICE_NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default socket path, matching `dist/manifest.toml`.
pub const DEFAULT_SOCKET: &str =
    "/var/lib/coolercontrol/plugins/gigabyte-gpu-rgb/gigabyte-gpu-rgb.sock";

#[allow(clippy::all, clippy::pedantic)]
pub mod models {
    pub mod v1 {
        tonic::include_proto!("coolercontrol.models.v1");
    }
}

#[allow(clippy::all, clippy::pedantic)]
pub mod device_service {
    pub mod v1 {
        tonic::include_proto!("coolercontrol.device_service.v1");
    }
}

use bus::LinuxI2cBus;
use discovery::FoundBus;
use log::{error, info, warn};
use service::GpuDevice;
use std::path::Path;

/// Opens and probes one discovered bus.
pub fn open_device(found: &FoundBus) -> anyhow::Result<GpuDevice> {
    let dev_path = found.dev_path();
    let mut bus = LinuxI2cBus::open(&dev_path)
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", dev_path.display()))?;
    let reply = bus::probe(&mut bus, found.card.address).map_err(|e| {
        anyhow::anyhow!(
            "probe of {} address 0x{:02x} failed: {e}",
            dev_path.display(),
            found.card.address
        )
    })?;
    info!(
        "Found {} at {} ({}, address 0x{:02x}), probe reply {}",
        found.card.name,
        found.pci_address,
        dev_path.display(),
        found.card.address,
        protocol::hex(&reply)
    );
    Ok(GpuDevice::new(
        found.pci_address.clone(),
        found.card.name.to_string(),
        found.key(),
        found.card.address,
        vec![
            dev_path.display().to_string(),
            found.sysfs_path.display().to_string(),
        ],
        Box::new(bus),
    ))
}

/// Finds and probes every supported card. Failures are logged, never fatal:
/// the result may be empty.
pub fn detect_devices(sysfs_root: &Path) -> Vec<GpuDevice> {
    let found = match discovery::discover_supported(sysfs_root) {
        Ok(f) => f,
        Err(e) => {
            error!("Cannot scan {}/bus/i2c/devices: {e}", sysfs_root.display());
            return Vec::new();
        }
    };
    if found.is_empty() {
        warn!(
            "No supported card found. Is the i2c-dev module loaded and is the card in the \
             supported list? Serving with no devices."
        );
    }
    found
        .iter()
        .filter_map(|f| match open_device(f) {
            Ok(d) => Some(d),
            Err(e) => {
                error!("{e}. Not listing this device.");
                None
            }
        })
        .collect()
}
