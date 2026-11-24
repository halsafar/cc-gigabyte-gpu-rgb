// SPDX-License-Identifier: GPL-3.0-or-later
//
// gRPC DeviceService implementation.
// Structure based on the CoolerControl device-service template (GPL-3.0-or-later).
//
// Only `Lighting` touches the I2C bus, plus the debounced auto-save it schedules.
// `Health`, `ListDevices`, `Status`, `InitializeDevice`, `ResetChannel` and
// `Shutdown` never do.

use crate::autosave::{AutoSave, StateStore};
use crate::bus::{self, I2cBus};
use crate::cards::CardKey;
use crate::device_service::v1::device_service_server::DeviceService;
use crate::device_service::v1::{
    CustomFunctionOneRequest, CustomFunctionOneResponse, EnableManualFanControlRequest,
    EnableManualFanControlResponse, FixedDutyRequest, FixedDutyResponse, HealthRequest,
    HealthResponse, InitializeDeviceRequest, InitializeDeviceResponse, LcdRequest, LcdResponse,
    LightingRequest, LightingResponse, LightingSetting, ListDevicesRequest, ListDevicesResponse,
    ResetChannelRequest, ResetChannelResponse, ShutdownRequest, ShutdownResponse,
    SpeedProfileRequest, SpeedProfileResponse, StatusRequest, StatusResponse, health_response,
};
use crate::models::v1::{
    ChannelInfo, Device, DeviceInfo, DriverInfo, LightingModes, channel_info,
    lighting_modes::LightingMode,
};
use crate::protocol::{self, Mode, Packet, Rgb, Speed};
use crate::{SERVICE_NAME, VERSION};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tonic::{Request, Response, Status};

/// The single lighting channel every device exposes.
pub const CHANNEL_ID: &str = "gpu";

/// A probed card, ready to receive lighting commands.
pub struct GpuDevice {
    /// Device id used on the gRPC API (the PCI address).
    pub id: String,
    pub name: String,
    /// PCI address and IDs; keys the saved-state record.
    pub key: CardKey,
    /// 7-bit I2C address of the controller.
    pub address: u16,
    /// Paths shown in CoolerControl's device details (device node, sysfs path).
    pub locations: Vec<String>,
    inner: Mutex<DeviceInner>,
}

/// Everything behind the per-device lock: the bus and what was last written to it.
struct DeviceInner {
    bus: Box<dyn I2cBus + Send>,
    /// Incremented on every apply attempt, successful or not.
    generation: u64,
    /// Packets of the last successful apply, i.e. what the card shows now.
    /// `None` after a failed apply (the card's state is then unknown).
    applied: Option<Vec<Packet>>,
    /// An apply succeeded and no save attempt has handled it yet.
    pending: bool,
}

/// Result of one compare-then-save attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// A newer apply happened since this attempt was scheduled; its own timer handles it.
    Superseded,
    /// No successful apply is waiting to be saved.
    NothingPending,
    /// The record says the card already holds this setting. Nothing written.
    Unchanged,
    /// The save packet was written.
    Saved,
    /// Writing the save packet failed. Not retried until the next apply.
    Failed,
}

impl GpuDevice {
    pub fn new(
        id: String,
        name: String,
        key: CardKey,
        address: u16,
        locations: Vec<String>,
        bus: Box<dyn I2cBus + Send>,
    ) -> Self {
        GpuDevice {
            id,
            name,
            key,
            address,
            locations,
            inner: Mutex::new(DeviceInner {
                bus,
                generation: 0,
                applied: None,
                pending: false,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DeviceInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes a lighting setting to the card. Blocking. Returns the apply's generation.
    pub fn apply(&self, mode: Mode, speed: Speed, color: Option<Rgb>) -> Result<u64, ApplyError> {
        let packets = protocol::encode(mode, speed, color).map_err(ApplyError::Encode)?;
        let mut inner = self.lock();
        inner.generation += 1;
        let address = self.address;
        match bus::write_packets(&mut *inner.bus, address, &packets) {
            Ok(()) => {
                inner.applied = Some(packets);
                inner.pending = true;
                Ok(inner.generation)
            }
            Err(e) => {
                // Part of the setting may have been written: never save this state.
                inner.applied = None;
                inner.pending = false;
                Err(ApplyError::Io(e))
            }
        }
    }

    /// Compare-then-save, under the same lock as `apply`, so it saves exactly what
    /// the card shows. `generation` is the apply that scheduled it (`None`: whatever
    /// is pending). Blocking.
    pub fn try_save(&self, store: &StateStore, generation: Option<u64>) -> SaveOutcome {
        let mut inner = self.lock();
        if generation.is_some_and(|g| g != inner.generation) {
            return SaveOutcome::Superseded;
        }
        if !inner.pending {
            return SaveOutcome::NothingPending;
        }
        inner.pending = false;
        let Some(applied) = inner.applied.clone() else {
            return SaveOutcome::NothingPending;
        };
        if store.load(&self.key).as_deref() == Some(applied.as_slice()) {
            debug!("{}: setting already saved on the card", self.id);
            return SaveOutcome::Unchanged;
        }
        let address = self.address;
        if let Err(e) = bus::write_packets(&mut *inner.bus, address, &protocol::encode_save()) {
            warn!("{}: saving to the card failed: {e}", self.id);
            return SaveOutcome::Failed;
        }
        info!(
            "{}: saved the lighting setting to the card's memory",
            self.id
        );
        if let Err(e) = store.store(&self.key, &applied) {
            warn!(
                "{}: saved, but cannot record it in {}: {e}",
                self.id,
                store.dir().display()
            );
        }
        SaveOutcome::Saved
    }

    fn model(&self) -> Device {
        let lighting_mode = Mode::ALL
            .iter()
            .map(|m| LightingMode {
                name: m.name().to_string(),
                frontend_name: Some(m.frontend_name().to_string()),
                min_colors: m.colors(),
                max_colors: m.colors(),
                speed_enabled: m.speed_enabled(),
                backward_enabled: false,
            })
            .collect();
        let channel = ChannelInfo {
            label: Some("GPU".to_string()),
            options: Some(channel_info::Options::LightingModes(LightingModes {
                lighting_mode,
            })),
        };
        Device {
            id: self.id.clone(),
            name: self.name.clone(),
            uid_info: Some(self.key.pci_address.clone()),
            info: Some(DeviceInfo {
                channels: HashMap::from([(CHANNEL_ID.to_string(), channel)]),
                temps: HashMap::new(),
                lighting_speeds: Speed::ALL.iter().map(|s| s.name().to_string()).collect(),
                temp_min: None,
                temp_max: None,
                profile_min_length: None,
                profile_max_length: None,
                model: Some(self.name.clone()),
                driver_info: Some(DriverInfo {
                    name: Some(SERVICE_NAME.to_string()),
                    version: Some(VERSION.to_string()),
                    locations: self.locations.clone(),
                }),
            }),
        }
    }
}

#[derive(Debug)]
pub enum ApplyError {
    Encode(protocol::EncodeError),
    Io(std::io::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Encode(e) => e.fmt(f),
            ApplyError::Io(e) => write!(f, "I2C write failed: {e}"),
        }
    }
}

impl std::error::Error for ApplyError {}

pub struct GigabyteGpuService {
    devices: Vec<Arc<GpuDevice>>,
    auto_save: Option<Arc<AutoSave>>,
    started: Instant,
}

/// Flushes pending saves at shutdown. Obtained before the service is moved into
/// the server.
#[derive(Clone)]
pub struct Flusher {
    devices: Vec<Arc<GpuDevice>>,
    auto_save: Option<Arc<AutoSave>>,
}

impl Flusher {
    /// Runs a compare-then-save for every device with a pending save, giving up after
    /// `timeout` so shutdown cannot hang.
    pub async fn flush(&self, timeout: Duration) {
        let Some(auto) = &self.auto_save else {
            return;
        };
        let work = async {
            for dev in &self.devices {
                let outcome = auto.run(Arc::clone(dev), None).await;
                if outcome == SaveOutcome::Saved {
                    info!("{}: pending save flushed at shutdown", dev.id);
                }
            }
        };
        if tokio::time::timeout(timeout, work).await.is_err() {
            warn!("Pending save not flushed within {timeout:?}; giving up");
        }
    }
}

impl GigabyteGpuService {
    /// A service without auto-save: it never writes the save packet.
    pub fn new(devices: Vec<GpuDevice>) -> Self {
        GigabyteGpuService {
            devices: devices.into_iter().map(Arc::new).collect(),
            auto_save: None,
            started: Instant::now(),
        }
    }

    pub fn with_auto_save(mut self, auto_save: Option<Arc<AutoSave>>) -> Self {
        self.auto_save = auto_save;
        self
    }

    pub fn flusher(&self) -> Flusher {
        Flusher {
            devices: self.devices.clone(),
            auto_save: self.auto_save.clone(),
        }
    }

    fn find(&self, device_id: &str) -> Result<&Arc<GpuDevice>, Status> {
        self.devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or_else(|| Status::not_found(format!("unknown device '{device_id}'")))
    }
}

/// A validated lighting request.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedSetting {
    pub mode: Mode,
    pub speed: Speed,
    pub color: Option<Rgb>,
}

/// Validates a `LightingSetting` from CoolerControl.
pub fn parse_setting(setting: &LightingSetting) -> Result<ParsedSetting, Status> {
    let mode = Mode::from_name(&setting.mode)
        .ok_or_else(|| Status::invalid_argument(format!("unknown mode '{}'", setting.mode)))?;

    // Speed only matters for modes that use it; it is ignored (not validated) otherwise.
    let speed = match (&setting.speed, mode.speed_enabled()) {
        (Some(s), true) if !s.is_empty() => Speed::from_name(s)
            .ok_or_else(|| Status::invalid_argument(format!("unknown speed '{s}'")))?,
        _ => Speed::Normal,
    };

    let color = if mode.colors() == 0 {
        None
    } else {
        let c = setting.colors.first().ok_or_else(|| {
            Status::invalid_argument(format!("mode '{mode}' requires one colour"))
        })?;
        let channel = |v: u32, which: &str| {
            u8::try_from(v).map_err(|_| {
                Status::invalid_argument(format!("colour {which} value {v} is out of range 0-255"))
            })
        };
        Some(Rgb::new(
            channel(c.r, "red")?,
            channel(c.g, "green")?,
            channel(c.b, "blue")?,
        ))
    };
    Ok(ParsedSetting { mode, speed, color })
}

#[tonic::async_trait]
impl DeviceService for GigabyteGpuService {
    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            name: SERVICE_NAME.to_string(),
            version: VERSION.to_string(),
            status: health_response::Status::Ok.into(),
            uptime_seconds: self.started.elapsed().as_secs(),
        }))
    }

    async fn list_devices(
        &self,
        _request: Request<ListDevicesRequest>,
    ) -> Result<Response<ListDevicesResponse>, Status> {
        Ok(Response::new(ListDevicesResponse {
            devices: self.devices.iter().map(|d| d.model()).collect(),
        }))
    }

    /// Called at start and after resume. The card needs no initialisation, and
    /// CoolerControl re-applies its saved lighting setting itself afterwards.
    async fn initialize_device(
        &self,
        request: Request<InitializeDeviceRequest>,
    ) -> Result<Response<InitializeDeviceResponse>, Status> {
        let dev = self.find(&request.get_ref().device_id)?;
        debug!("InitializeDevice {}", dev.id);
        Ok(Response::new(InitializeDeviceResponse {}))
    }

    /// Does not exit: this service is run by systemd, not by CoolerControl, and must
    /// still be there when the daemon starts again.
    async fn shutdown(
        &self,
        _request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        info!("CoolerControl sent Shutdown; staying up for the next connection");
        Ok(Response::new(ShutdownResponse {}))
    }

    /// No sensors: always empty, never touches I2C.
    async fn status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        Ok(Response::new(StatusResponse { status: Vec::new() }))
    }

    async fn reset_channel(
        &self,
        _request: Request<ResetChannelRequest>,
    ) -> Result<Response<ResetChannelResponse>, Status> {
        Ok(Response::new(ResetChannelResponse {}))
    }

    async fn enable_manual_fan_control(
        &self,
        _request: Request<EnableManualFanControlRequest>,
    ) -> Result<Response<EnableManualFanControlResponse>, Status> {
        Err(Status::unimplemented("No Fans"))
    }

    async fn fixed_duty(
        &self,
        _request: Request<FixedDutyRequest>,
    ) -> Result<Response<FixedDutyResponse>, Status> {
        Err(Status::unimplemented("No Fans"))
    }

    async fn speed_profile(
        &self,
        _request: Request<SpeedProfileRequest>,
    ) -> Result<Response<SpeedProfileResponse>, Status> {
        Err(Status::unimplemented("No Firmware Profiles"))
    }

    async fn lighting(
        &self,
        request: Request<LightingRequest>,
    ) -> Result<Response<LightingResponse>, Status> {
        let req = request.into_inner();
        let dev = Arc::clone(self.find(&req.device_id)?);
        if req.channel_id != CHANNEL_ID {
            return Err(Status::not_found(format!(
                "unknown channel '{}' (only '{CHANNEL_ID}')",
                req.channel_id
            )));
        }
        let setting = req
            .setting
            .ok_or_else(|| Status::invalid_argument("missing lighting setting"))?;
        if setting.backward == Some(true) {
            debug!("ignoring backward=true: the card has no direction control");
        }
        let ParsedSetting { mode, speed, color } = parse_setting(&setting)?;
        let color_txt = color.map(|c| format!(" #{c}")).unwrap_or_default();
        info!("{}: applying {mode} ({speed}){color_txt}", dev.id);

        let applying = Arc::clone(&dev);
        let result = tokio::task::spawn_blocking(move || applying.apply(mode, speed, color))
            .await
            .map_err(|e| Status::internal(format!("lighting task failed: {e}")))?;
        match result {
            Ok(generation) => {
                if let Some(auto) = &self.auto_save {
                    auto.schedule(dev, generation);
                }
                Ok(Response::new(LightingResponse {}))
            }
            Err(ApplyError::Encode(e)) => Err(Status::invalid_argument(e.to_string())),
            Err(e @ ApplyError::Io(_)) => {
                warn!("{e}");
                Err(Status::unavailable(e.to_string()))
            }
        }
    }

    async fn lcd(&self, _request: Request<LcdRequest>) -> Result<Response<LcdResponse>, Status> {
        Err(Status::unimplemented("No LCD Channels"))
    }

    async fn custom_function_one(
        &self,
        _request: Request<CustomFunctionOneRequest>,
    ) -> Result<Response<CustomFunctionOneResponse>, Status> {
        Err(Status::unimplemented("No Custom Function"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_service::v1::Rgb as PbRgb;

    fn setting(mode: &str, speed: Option<&str>, colors: &[(u32, u32, u32)]) -> LightingSetting {
        LightingSetting {
            mode: mode.to_string(),
            speed: speed.map(str::to_string),
            backward: None,
            colors: colors.iter().map(|&(r, g, b)| PbRgb { r, g, b }).collect(),
        }
    }

    #[test]
    fn parses_valid_settings() {
        assert_eq!(
            parse_setting(&setting("breathing", Some("fastest"), &[(1, 2, 3)])).unwrap(),
            ParsedSetting {
                mode: Mode::Breathing,
                speed: Speed::Fastest,
                color: Some(Rgb::new(1, 2, 3))
            }
        );
        // No speed: default.
        assert_eq!(
            parse_setting(&setting("color_cycle", None, &[])).unwrap(),
            ParsedSetting {
                mode: Mode::ColorCycle,
                speed: Speed::Normal,
                color: None
            }
        );
        // Speed on a speed-less mode is ignored, even if unknown.
        assert_eq!(
            parse_setting(&setting("static", Some("whatever"), &[(9, 9, 9)]))
                .unwrap()
                .speed,
            Speed::Normal
        );
        // Extra colours are ignored.
        assert_eq!(
            parse_setting(&setting("static", None, &[(1, 1, 1), (2, 2, 2)]))
                .unwrap()
                .color,
            Some(Rgb::new(1, 1, 1))
        );
    }

    #[test]
    fn rejects_invalid_settings() {
        let code = |s: LightingSetting| parse_setting(&s).unwrap_err().code();
        assert_eq!(
            code(setting("disco", None, &[])),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            code(setting("static", None, &[])),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            code(setting("flashing", Some("warp"), &[(1, 1, 1)])),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            code(setting("static", None, &[(256, 0, 0)])),
            tonic::Code::InvalidArgument
        );
    }
}
