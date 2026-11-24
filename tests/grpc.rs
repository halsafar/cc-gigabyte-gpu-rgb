// SPDX-License-Identifier: GPL-3.0-or-later
//
// End-to-end test: the real gRPC server on a Unix socket, a tonic client, and a
// mock I2C bus in place of /dev/i2c-N.

use cc_gigabyte_gpu_rgb::autosave::{AutoSave, StateStore};
use cc_gigabyte_gpu_rgb::bus::mock::{MockBus, Op};
use cc_gigabyte_gpu_rgb::cards::{CardKey, SUPPORTED_CARDS};
use cc_gigabyte_gpu_rgb::device_service::v1::device_service_client::DeviceServiceClient;
use cc_gigabyte_gpu_rgb::device_service::v1::{
    FixedDutyRequest, HealthRequest, InitializeDeviceRequest, LightingRequest, LightingSetting,
    ListDevicesRequest, ResetChannelRequest, Rgb, ShutdownRequest, StatusRequest, health_response,
};
use cc_gigabyte_gpu_rgb::models::v1::channel_info::Options;
use cc_gigabyte_gpu_rgb::protocol::{self, Mode, Rgb as Color, SAVE_PACKET, Speed};
use cc_gigabyte_gpu_rgb::server::serve_uds;
use cc_gigabyte_gpu_rgb::service::{GigabyteGpuService, GpuDevice};
use hyper_util::rt::TokioIo;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tonic::Code;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

const PCI: &str = "0000:04:00.0";

struct Harness {
    client: DeviceServiceClient<Channel>,
    stop: Option<oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn start(devices: Vec<GpuDevice>) -> Self {
        Self::start_service(GigabyteGpuService::new(devices)).await
    }

    async fn start_service(service: GigabyteGpuService) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("plugin.sock");
        // A stale socket from a previous run must not prevent startup.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

        let (tx, rx) = oneshot::channel::<()>();
        let sock = socket.clone();
        let server = tokio::spawn(async move {
            serve_uds(service, &sock, async {
                let _ = rx.await;
            })
            .await
        });

        let sock = socket.clone();
        let mut last_err = None;
        for _ in 0..100 {
            let sock = sock.clone();
            match Endpoint::try_from("http://[::]:50051")
                .unwrap()
                .connect_with_connector(service_fn(move |_: Uri| {
                    let sock = sock.clone();
                    async move {
                        Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(sock).await?))
                    }
                }))
                .await
            {
                Ok(channel) => {
                    return Harness {
                        client: DeviceServiceClient::new(channel),
                        stop: Some(tx),
                        server,
                        socket,
                        _dir: dir,
                    };
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
        panic!("server did not come up: {last_err:?}");
    }

    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        drop(self.client);
        self.server.await.unwrap().unwrap();
        assert!(!self.socket.exists(), "socket must be removed on shutdown");
    }
}

fn key() -> CardKey {
    CardKey {
        pci_address: PCI.to_string(),
        ids: SUPPORTED_CARDS[0].ids(),
    }
}

fn device(bus: &MockBus) -> GpuDevice {
    GpuDevice::new(
        PCI.to_string(),
        "Gigabyte Radeon RX 7700 XT GAMING OC".to_string(),
        key(),
        0x62,
        vec!["/dev/i2c-21".to_string()],
        Box::new(bus.clone()),
    )
}

fn lighting(mode: &str, speed: Option<&str>, colors: &[(u32, u32, u32)]) -> LightingRequest {
    LightingRequest {
        device_id: PCI.to_string(),
        channel_id: "gpu".to_string(),
        setting: Some(LightingSetting {
            mode: mode.to_string(),
            speed: speed.map(str::to_string),
            backward: Some(false),
            colors: colors.iter().map(|&(r, g, b)| Rgb { r, g, b }).collect(),
        }),
    }
}

#[tokio::test]
async fn list_devices_and_read_only_calls_do_not_touch_i2c() {
    let bus = MockBus::new();
    let mut h = Harness::start(vec![device(&bus)]).await;

    let health = h
        .client
        .health(HealthRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(health.name, "cc-gigabyte-gpu-rgb");
    assert_eq!(health.status, health_response::Status::Ok as i32);

    let devices = h
        .client
        .list_devices(ListDevicesRequest {})
        .await
        .unwrap()
        .into_inner()
        .devices;
    assert_eq!(devices.len(), 1);
    let d = &devices[0];
    assert_eq!(d.id, PCI);
    assert_eq!(d.name, "Gigabyte Radeon RX 7700 XT GAMING OC");
    assert_eq!(d.uid_info.as_deref(), Some(PCI));
    let info = d.info.as_ref().unwrap();
    assert_eq!(
        info.lighting_speeds,
        vec!["slowest", "slower", "normal", "faster", "fastest"]
    );
    assert!(info.temps.is_empty());
    assert_eq!(info.channels.len(), 1);
    let Some(Options::LightingModes(modes)) = &info.channels["gpu"].options else {
        panic!("gpu channel must be a lighting channel");
    };
    let summary: Vec<(&str, u32, u32, bool, bool)> = modes
        .lighting_mode
        .iter()
        .map(|m| {
            (
                m.name.as_str(),
                m.min_colors,
                m.max_colors,
                m.speed_enabled,
                m.backward_enabled,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("static", 1, 1, false, false),
            ("breathing", 1, 1, true, false),
            ("flashing", 1, 1, true, false),
            ("double_flash", 1, 1, true, false),
            ("gradient", 1, 1, true, false),
            ("color_cycle", 0, 0, true, false),
            ("off", 0, 0, false, false),
        ]
    );

    let status = h
        .client
        .status(StatusRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(status.status.is_empty());
    h.client
        .initialize_device(InitializeDeviceRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap();
    h.client
        .reset_channel(ResetChannelRequest {
            device_id: PCI.to_string(),
            channel_id: "gpu".to_string(),
        })
        .await
        .unwrap();
    h.client.shutdown(ShutdownRequest {}).await.unwrap();

    assert_eq!(bus.ops(), vec![], "no I2C traffic expected");

    let err = h
        .client
        .fixed_duty(FixedDutyRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unimplemented);

    // Shutdown RPC must not stop the server.
    h.client.health(HealthRequest {}).await.unwrap();
    h.stop().await;
}

#[tokio::test]
async fn lighting_writes_expected_packets() {
    let bus = MockBus::new();
    let mut h = Harness::start(vec![device(&bus)]).await;

    h.client
        .lighting(lighting("breathing", Some("fastest"), &[(255, 0, 16)]))
        .await
        .unwrap();
    let ops = bus.ops();
    assert_eq!(ops.len(), 10);
    assert!(ops.iter().all(|op| matches!(op, Op::Write(0x62, _))));
    assert_eq!(
        bus.writes(),
        vec![
            vec![0x88, 0x02, 0x05, 0x63, 0x00, 1, 0, 0],
            vec![0xB0, 0x02, 0xff, 0x00, 0x10, 0xff, 0x00, 0x10],
            vec![0x88, 0x02, 0x05, 0x63, 0x00, 2, 0, 0],
            vec![0xB0, 0x02, 0xff, 0x00, 0x10, 0xff, 0x00, 0x10],
            vec![0x88, 0x02, 0x05, 0x63, 0x00, 3, 0, 0],
            vec![0xB1, 0x02, 0xff, 0x00, 0x10, 0, 0, 0],
            vec![0x88, 0x02, 0x05, 0x63, 0x00, 4, 0, 0],
            vec![0x40, 0xff, 0x00, 0x10, 4, 0, 0, 0],
            vec![0x88, 0x02, 0x05, 0x63, 0x00, 5, 0, 0],
            vec![0x40, 0xff, 0x00, 0x10, 5, 0, 0, 0],
        ]
    );

    bus.clear();
    h.client
        .lighting(lighting("color_cycle", Some("slowest"), &[]))
        .await
        .unwrap();
    assert_eq!(
        bus.writes(),
        (1..=5)
            .map(|z| vec![0x88, 0x03, 0x00, 0x63, 0x00, z, 0, 0])
            .collect::<Vec<_>>()
    );

    bus.clear();
    h.client.lighting(lighting("off", None, &[])).await.unwrap();
    let w = bus.writes();
    assert_eq!(w.len(), 10);
    assert_eq!(w[1], vec![0xB0, 0x01, 0, 0, 0, 0, 0, 0]);

    h.stop().await;
}

#[tokio::test]
async fn invalid_lighting_requests_are_rejected_without_io() {
    let bus = MockBus::new();
    let mut h = Harness::start(vec![device(&bus)]).await;

    let mut unknown_device = lighting("static", None, &[(1, 2, 3)]);
    unknown_device.device_id = "nope".to_string();
    let mut unknown_channel = lighting("static", None, &[(1, 2, 3)]);
    unknown_channel.channel_id = "led1".to_string();
    let mut no_setting = lighting("static", None, &[]);
    no_setting.setting = None;

    let cases = [
        (unknown_device, Code::NotFound),
        (unknown_channel, Code::NotFound),
        (no_setting, Code::InvalidArgument),
        (lighting("rainbow_puke", None, &[]), Code::InvalidArgument),
        (lighting("wave", None, &[]), Code::InvalidArgument),
        (lighting("static", None, &[]), Code::InvalidArgument),
        (
            lighting("gradient", Some("warp9"), &[(1, 1, 1)]),
            Code::InvalidArgument,
        ),
        (
            lighting("static", None, &[(300, 0, 0)]),
            Code::InvalidArgument,
        ),
    ];
    for (req, code) in cases {
        let err = h.client.lighting(req.clone()).await.unwrap_err();
        assert_eq!(err.code(), code, "{req:?}: {}", err.message());
    }
    assert_eq!(
        bus.ops(),
        vec![],
        "rejected requests must not reach the bus"
    );

    let err = h
        .client
        .initialize_device(InitializeDeviceRequest {
            device_id: "nope".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);

    // Bus failure surfaces as an error, not a panic, and the server keeps running.
    bus.fail_writes(true);
    let err = h
        .client
        .lighting(lighting("static", None, &[(1, 2, 3)]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unavailable);
    bus.fail_writes(false);
    h.client
        .lighting(lighting("static", None, &[(1, 2, 3)]))
        .await
        .unwrap();

    h.stop().await;
}

#[tokio::test]
async fn service_without_auto_save_never_sends_save_packet() {
    let bus = MockBus::new();
    let mut h = Harness::start(vec![device(&bus)]).await;

    // Every exposed mode at every speed, plus every other RPC that takes a device.
    for m in Mode::ALL {
        for s in Speed::ALL {
            let colors: &[(u32, u32, u32)] = if m.colors() > 0 { &[(1, 2, 3)] } else { &[] };
            h.client
                .lighting(lighting(m.name(), Some(s.name()), colors))
                .await
                .unwrap();
        }
    }
    h.client
        .initialize_device(InitializeDeviceRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap();
    h.client
        .reset_channel(ResetChannelRequest {
            device_id: PCI.to_string(),
            channel_id: "gpu".to_string(),
        })
        .await
        .unwrap();
    h.client.shutdown(ShutdownRequest {}).await.unwrap();

    let writes = bus.writes();
    assert!(!writes.is_empty());
    assert!(
        writes.iter().all(|w| w[0] != SAVE_PACKET[0]),
        "the service must never write the save packet"
    );
    h.stop().await;
}

fn count_saves(bus: &MockBus) -> usize {
    bus.writes()
        .iter()
        .filter(|w| w[0] == SAVE_PACKET[0])
        .count()
}

/// Over the real socket: with auto-save on, nothing but a real change writes `AA`.
#[tokio::test]
async fn auto_save_only_on_change_and_never_from_other_rpcs() {
    let bus = MockBus::new();
    let state = tempfile::tempdir().unwrap();
    let store = StateStore::new(state.path());
    // The card already holds static 010203.
    let saved = protocol::encode(Mode::Static, Speed::Normal, Some(Color::new(1, 2, 3))).unwrap();
    store.store(&key(), &saved).unwrap();
    let auto = AutoSave::new(store.clone(), Duration::from_millis(100));
    let service = GigabyteGpuService::new(vec![device(&bus)]).with_auto_save(Some(auto));
    let mut h = Harness::start_service(service).await;

    // The same setting again (as after a daemon restart or resume), also with
    // speeds that static ignores: no save.
    for s in Speed::ALL {
        h.client
            .lighting(lighting("static", Some(s.name()), &[(1, 2, 3)]))
            .await
            .unwrap();
    }
    h.client
        .initialize_device(InitializeDeviceRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap();
    h.client
        .reset_channel(ResetChannelRequest {
            device_id: PCI.to_string(),
            channel_id: "gpu".to_string(),
        })
        .await
        .unwrap();
    h.client
        .status(StatusRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap();
    h.client.list_devices(ListDevicesRequest {}).await.unwrap();
    h.client.health(HealthRequest {}).await.unwrap();
    h.client.shutdown(ShutdownRequest {}).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(count_saves(&bus), 0);
    assert_eq!(bus.writes().len(), 5 * 10, "only the five lighting applies");

    // A real change: exactly one save, after the delay, as the last write.
    h.client
        .lighting(lighting("static", None, &[(4, 5, 6)]))
        .await
        .unwrap();
    assert_eq!(count_saves(&bus), 0, "nothing before the delay");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(count_saves(&bus), 1);
    assert_eq!(bus.writes().last().unwrap(), &SAVE_PACKET.to_vec());
    assert_eq!(
        store.load(&key()),
        Some(protocol::encode(Mode::Static, Speed::Normal, Some(Color::new(4, 5, 6))).unwrap())
    );

    // Idle, and non-lighting RPCs: no I2C traffic at all.
    let before = bus.ops().len();
    h.client.health(HealthRequest {}).await.unwrap();
    h.client
        .status(StatusRequest {
            device_id: PCI.to_string(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bus.ops().len(), before);
    h.stop().await;
}

#[tokio::test]
async fn zero_devices_still_serves() {
    let mut h = Harness::start(Vec::new()).await;
    let devices = h
        .client
        .list_devices(ListDevicesRequest {})
        .await
        .unwrap()
        .into_inner()
        .devices;
    assert!(devices.is_empty());
    let err = h
        .client
        .lighting(lighting("static", None, &[(1, 2, 3)]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::NotFound);
    h.stop().await;
}
