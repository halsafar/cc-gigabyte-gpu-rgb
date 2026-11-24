// SPDX-License-Identifier: GPL-3.0-or-later
//
// Auto-save behaviour of the service, with a mock bus and paused tokio time.
// The RPCs are called directly on the service (no socket), so virtual time can be
// advanced freely.

use cc_gigabyte_gpu_rgb::autosave::{self, AutoSave, StateStore};
use cc_gigabyte_gpu_rgb::bus::mock::MockBus;
use cc_gigabyte_gpu_rgb::cards::{CardKey, SUPPORTED_CARDS};
use cc_gigabyte_gpu_rgb::device_service::v1::device_service_server::DeviceService;
use cc_gigabyte_gpu_rgb::device_service::v1::{
    HealthRequest, InitializeDeviceRequest, LightingRequest, LightingSetting, ListDevicesRequest,
    ResetChannelRequest, Rgb, ShutdownRequest, StatusRequest,
};
use cc_gigabyte_gpu_rgb::protocol::{self, Mode, Packet, Rgb as Color, SAVE_PACKET, Speed};
use cc_gigabyte_gpu_rgb::service::{GigabyteGpuService, GpuDevice};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tonic::Request;

const PCI: &str = "0000:04:00.0";
const DELAY: Duration = Duration::from_secs(30);

fn key() -> CardKey {
    CardKey {
        pci_address: PCI.to_string(),
        ids: SUPPORTED_CARDS[0].ids(),
    }
}

struct Rig {
    bus: MockBus,
    svc: GigabyteGpuService,
    auto: Option<Arc<AutoSave>>,
    store: Option<StateStore>,
}

impl Rig {
    fn new(state_dir: Option<&Path>) -> Self {
        let bus = MockBus::new();
        let dev = GpuDevice::new(
            PCI.to_string(),
            "test card".to_string(),
            key(),
            0x62,
            vec![],
            Box::new(bus.clone()),
        );
        let store = state_dir.map(StateStore::new);
        let auto = store.clone().map(|s| AutoSave::new(s, DELAY));
        let svc = GigabyteGpuService::new(vec![dev]).with_auto_save(auto.clone());
        Rig {
            bus,
            svc,
            auto,
            store,
        }
    }

    async fn apply(&self, mode: &str, speed: Option<&str>, color: Option<(u32, u32, u32)>) {
        self.try_apply(mode, speed, color).await.unwrap();
    }

    async fn try_apply(
        &self,
        mode: &str,
        speed: Option<&str>,
        color: Option<(u32, u32, u32)>,
    ) -> Result<(), tonic::Status> {
        self.svc
            .lighting(Request::new(LightingRequest {
                device_id: PCI.to_string(),
                channel_id: "gpu".to_string(),
                setting: Some(LightingSetting {
                    mode: mode.to_string(),
                    speed: speed.map(str::to_string),
                    backward: None,
                    colors: color.map(|(r, g, b)| Rgb { r, g, b }).into_iter().collect(),
                }),
            }))
            .await
            .map(|_| ())
    }

    fn saves(&self) -> usize {
        self.bus
            .writes()
            .iter()
            .filter(|w| w[0] == SAVE_PACKET[0])
            .count()
    }

    /// Waits until `n` save attempts (any outcome) have finished.
    async fn attempts(&self, n: u64) {
        let mut rx = self.auto.as_ref().unwrap().completed();
        tokio::time::timeout(Duration::from_secs(3600), rx.wait_for(|c| *c >= n))
            .await
            .expect("save attempts did not finish")
            .unwrap();
    }

    fn attempts_now(&self) -> u64 {
        *self.auto.as_ref().unwrap().completed().borrow()
    }

    fn record(&self) -> Option<Vec<Packet>> {
        self.store.as_ref().unwrap().load(&key())
    }
}

fn enc(mode: Mode, speed: Speed, c: Option<(u8, u8, u8)>) -> Vec<Packet> {
    protocol::encode(mode, speed, c.map(|(r, g, b)| Color::new(r, g, b))).unwrap()
}

#[tokio::test(start_paused = true)]
async fn one_apply_saves_once_after_the_delay() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    rig.apply("static", None, Some((0, 255, 0))).await;
    assert_eq!(rig.bus.writes().len(), 10);
    sleep(DELAY - Duration::from_secs(1)).await;
    assert_eq!(rig.saves(), 0, "nothing before the delay");
    assert_eq!(rig.attempts_now(), 0);
    sleep(Duration::from_secs(2)).await;
    rig.attempts(1).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(rig.bus.writes().last().unwrap(), &SAVE_PACKET.to_vec());
    assert_eq!(
        rig.record(),
        Some(enc(Mode::Static, Speed::Normal, Some((0, 255, 0))))
    );
    // Nothing more happens while nothing changes.
    let ops = rig.bus.ops().len();
    sleep(Duration::from_secs(3600)).await;
    assert_eq!(rig.bus.ops().len(), ops);
}

#[tokio::test(start_paused = true)]
async fn clicking_through_colours_costs_one_save() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    rig.apply("static", None, Some((255, 0, 0))).await;
    sleep(Duration::from_secs(10)).await;
    // A rejected request changes nothing and schedules nothing.
    assert!(
        rig.try_apply("breathing", Some("fast"), Some((0, 255, 0)))
            .await
            .is_err()
    );
    rig.apply("breathing", Some("faster"), Some((0, 255, 0)))
        .await;
    sleep(Duration::from_secs(25)).await;
    assert_eq!(rig.saves(), 0, "the timer restarts on every apply");
    rig.apply("static", None, Some((0, 0, 255))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(3).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(
        rig.record(),
        Some(enc(Mode::Static, Speed::Normal, Some((0, 0, 255))))
    );
}

#[tokio::test(start_paused = true)]
async fn reapplying_the_saved_setting_costs_nothing_even_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let rig = Rig::new(Some(dir.path()));
        rig.apply("gradient", Some("slower"), Some((1, 2, 3))).await;
        sleep(DELAY + Duration::from_secs(1)).await;
        rig.attempts(1).await;
        assert_eq!(rig.saves(), 1);
    }
    // A fresh service over the same state directory: a service restart or reboot,
    // after which CoolerControl re-sends the same setting.
    let rig = Rig::new(Some(dir.path()));
    rig.apply("gradient", Some("slower"), Some((1, 2, 3))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    // Resume: the same setting once more.
    rig.apply("gradient", Some("slower"), Some((1, 2, 3))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(2).await;
    assert_eq!(rig.saves(), 0);
    assert_eq!(rig.bus.writes().len(), 20);
}

#[tokio::test(start_paused = true)]
async fn equivalent_requests_do_not_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path());
    store
        .store(&key(), &enc(Mode::Static, Speed::Normal, Some((9, 9, 9))))
        .unwrap();
    let rig = Rig::new(Some(dir.path()));
    // Static ignores speed; extra colours are ignored.
    rig.apply("static", Some("fastest"), Some((9, 9, 9))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    assert_eq!(rig.saves(), 0);

    // `off` and static black encode identically.
    store
        .store(&key(), &enc(Mode::Off, Speed::Normal, None))
        .unwrap();
    rig.apply("static", Some("slowest"), Some((0, 0, 0))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(2).await;
    assert_eq!(rig.saves(), 0);
}

#[tokio::test(start_paused = true)]
async fn failed_save_leaves_record_and_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    let old = enc(Mode::Static, Speed::Normal, Some((1, 1, 1)));
    rig.store.as_ref().unwrap().store(&key(), &old).unwrap();

    rig.apply("static", None, Some((2, 2, 2))).await;
    rig.bus.fail_writes(true);
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    assert_eq!(rig.record(), Some(old.clone()), "record unchanged");
    // No retry loop: nothing else is attempted, however long we wait.
    sleep(Duration::from_secs(24 * 3600)).await;
    assert_eq!(rig.attempts_now(), 1);
    assert_eq!(rig.saves(), 0);

    // The next apply schedules again and saves.
    rig.bus.fail_writes(false);
    rig.apply("static", None, Some((2, 2, 2))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(2).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(
        rig.record(),
        Some(enc(Mode::Static, Speed::Normal, Some((2, 2, 2))))
    );
}

#[tokio::test(start_paused = true)]
async fn failed_apply_cancels_the_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    rig.apply("static", None, Some((1, 1, 1))).await;
    rig.bus.fail_writes(true);
    assert_eq!(
        rig.try_apply("static", None, Some((2, 2, 2)))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unavailable
    );
    rig.bus.fail_writes(false);
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    // The card may hold half of the second setting: never save that.
    assert_eq!(rig.saves(), 0);
    assert_eq!(rig.record(), None);
}

#[tokio::test(start_paused = true)]
async fn corrupt_or_missing_record_means_one_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path());
    std::fs::write(store.path_for(&key()), "not a record\n").unwrap();
    let rig = Rig::new(Some(dir.path()));
    rig.apply("color_cycle", Some("fastest"), None).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(
        rig.record(),
        Some(enc(Mode::ColorCycle, Speed::Fastest, None))
    );
}

#[tokio::test(start_paused = true)]
async fn record_that_cannot_be_written_still_saves() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let rig = Rig::new(Some(&state));
    // Make the record path a non-empty directory: rename onto it fails.
    let p = rig.store.as_ref().unwrap().path_for(&key());
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("x"), "").unwrap();
    rig.apply("static", None, Some((5, 5, 5))).await;
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(1).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(rig.record(), None);
}

#[tokio::test(start_paused = true)]
async fn auto_save_off_never_saves() {
    let rig = Rig::new(None);
    rig.apply("static", None, Some((1, 2, 3))).await;
    rig.apply("breathing", None, Some((3, 2, 1))).await;
    sleep(Duration::from_secs(24 * 3600)).await;
    assert_eq!(rig.saves(), 0);
    rig.svc.flusher().flush(Duration::from_secs(5)).await;
    assert_eq!(rig.saves(), 0);
    // --no-auto-save, no state directory: no AutoSave at all.
    let dir = tempfile::tempdir().unwrap();
    assert!(autosave::configure(true, Some(dir.path()), None, DELAY).is_none());
    assert!(autosave::configure(false, None, None, DELAY).is_none());
    // Otherwise on: --state-dir wins over $STATE_DIRECTORY; the first of a list is used.
    let a = autosave::configure(
        false,
        Some(dir.path()),
        Some("/nonexistent".as_ref()),
        DELAY,
    )
    .unwrap();
    assert_eq!(a.store().dir(), dir.path());
    let list = format!("{}:/other", dir.path().display());
    let b = autosave::configure(false, None, Some(list.as_ref()), DELAY).unwrap();
    assert_eq!(b.store().dir(), dir.path());
    assert_eq!(b.delay(), DELAY);
}

#[tokio::test(start_paused = true)]
async fn shutdown_flushes_a_pending_save() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    rig.apply("flashing", Some("slowest"), Some((7, 7, 7)))
        .await;
    let flusher = rig.svc.flusher();
    flusher.flush(Duration::from_secs(5)).await;
    assert_eq!(rig.saves(), 1);
    assert_eq!(
        rig.record(),
        Some(enc(Mode::Flashing, Speed::Slowest, Some((7, 7, 7))))
    );
    // The timer firing later finds nothing pending.
    sleep(DELAY + Duration::from_secs(1)).await;
    rig.attempts(2).await;
    assert_eq!(rig.saves(), 1);
    // Flushing with nothing pending writes nothing.
    flusher.flush(Duration::from_secs(5)).await;
    assert_eq!(rig.saves(), 1);
}

#[tokio::test(start_paused = true)]
async fn non_lighting_rpcs_never_touch_i2c() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(Some(dir.path()));
    let id = || PCI.to_string();
    for _ in 0..3 {
        rig.svc
            .health(Request::new(HealthRequest {}))
            .await
            .unwrap();
        rig.svc
            .list_devices(Request::new(ListDevicesRequest {}))
            .await
            .unwrap();
        rig.svc
            .status(Request::new(StatusRequest { device_id: id() }))
            .await
            .unwrap();
        rig.svc
            .initialize_device(Request::new(InitializeDeviceRequest { device_id: id() }))
            .await
            .unwrap();
        rig.svc
            .reset_channel(Request::new(ResetChannelRequest {
                device_id: id(),
                channel_id: "gpu".to_string(),
            }))
            .await
            .unwrap();
        rig.svc
            .shutdown(Request::new(ShutdownRequest {}))
            .await
            .unwrap();
    }
    sleep(Duration::from_secs(3600)).await;
    rig.svc.flusher().flush(Duration::from_secs(5)).await;
    assert!(rig.bus.ops().is_empty());
}

#[test]
fn cli_record_functions() {
    // set --save records; plain save forgets.
    let dir = tempfile::tempdir().unwrap();
    let p = enc(Mode::Static, Speed::Normal, Some((1, 2, 3)));
    assert!(autosave::cli_record_saved(dir.path(), &key(), &p).unwrap());
    assert_eq!(StateStore::new(dir.path()).load(&key()), Some(p));
    assert!(autosave::cli_forget(dir.path(), &key()).unwrap());
    assert_eq!(StateStore::new(dir.path()).load(&key()), None);
}
