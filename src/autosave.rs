// SPDX-License-Identifier: GPL-3.0-or-later
//
// Debounced, change-only saving of the lighting setting to the card's memory.
//
// The card cannot be read back, so the plugin keeps a record of what it believes is
// stored on each card: one small file per card in a state directory, holding the
// encoded packets of the last saved setting. A save is only written when the setting
// that has been on the card for `delay` differs from that record.

use crate::cards::CardKey;
use crate::protocol::Packet;
use crate::service::{GpuDevice, SaveOutcome};
use log::{debug, warn};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Default directory, matching `StateDirectory=cc-gigabyte-gpu-rgb` in the unit.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/cc-gigabyte-gpu-rgb";
pub const DEFAULT_DELAY: Duration = Duration::from_secs(30);

const HEADER: &str = "cc-gigabyte-gpu-rgb saved-state v1";

/// Per-card records of the setting believed to be saved on the card.
#[derive(Clone, Debug)]
pub struct StateStore {
    dir: PathBuf,
}

impl StateStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        StateStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<pci address>_<vendor>-<device>-<subsys vendor>-<subsys device>.saved`
    pub fn path_for(&self, key: &CardKey) -> PathBuf {
        let pci: String = key
            .pci_address
            .chars()
            .map(|c| {
                if c.is_ascii_hexdigit() || c == ':' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let i = &key.ids;
        self.dir.join(format!(
            "{pci}_{:04x}-{:04x}-{:04x}-{:04x}.saved",
            i.vendor, i.device, i.subsystem_vendor, i.subsystem_device
        ))
    }

    /// The recorded setting, or `None` for "unknown" (missing, unreadable or invalid).
    pub fn load(&self, key: &CardKey) -> Option<Vec<Packet>> {
        let path = self.path_for(key);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = parse(&text);
                if parsed.is_none() {
                    warn!("Ignoring invalid saved-state record {}", path.display());
                }
                parsed
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                warn!("Cannot read {}: {e}", path.display());
                None
            }
        }
    }

    /// Records `packets` as saved on the card. Atomic: temp file, fsync, rename.
    pub fn store(&self, key: &CardKey, packets: &[Packet]) -> io::Result<()> {
        let path = self.path_for(key);
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(format(packets).as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &path)?;
            if let Ok(d) = fs::File::open(&self.dir) {
                let _ = d.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Forgets the record, so the card's memory counts as unknown.
    pub fn clear(&self, key: &CardKey) -> io::Result<()> {
        match fs::remove_file(self.path_for(key)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

fn format(packets: &[Packet]) -> String {
    let mut s = String::from(HEADER);
    s.push('\n');
    for p in packets {
        s.push_str(&crate::protocol::hex(p));
        s.push('\n');
    }
    s
}

fn parse(text: &str) -> Option<Vec<Packet>> {
    let mut lines = text.lines();
    if lines.next()? != HEADER {
        return None;
    }
    let mut packets = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let bytes: Vec<u8> = line
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).ok())
            .collect::<Option<_>>()?;
        packets.push(<Packet>::try_from(bytes.as_slice()).ok()?);
    }
    if packets.is_empty() {
        None
    } else {
        Some(packets)
    }
}

/// `set --save`: the CLI knows what it saved, so it records it.
/// Returns `Ok(false)` if the state directory does not exist (nothing to update).
pub fn cli_record_saved(dir: &Path, key: &CardKey, packets: &[Packet]) -> io::Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    StateStore::new(dir).store(key, packets)?;
    Ok(true)
}

/// Plain `save`: the CLI does not know what the card shows, so the record is dropped
/// and the service's belief becomes "unknown". `Ok(false)` if the directory is missing.
pub fn cli_forget(dir: &Path, key: &CardKey) -> io::Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    StateStore::new(dir).clear(key)?;
    Ok(true)
}

/// Decides whether the service auto-saves and where its records live:
/// `--state-dir` (`flag`), else `$STATE_DIRECTORY` (`env`, the first of a
/// `:`-separated list). Without either, or with `disabled`, auto-save is off. A
/// directory that does not exist is created; if that fails, auto-save is off.
pub fn configure(
    disabled: bool,
    flag: Option<&Path>,
    env: Option<&std::ffi::OsStr>,
    delay: Duration,
) -> Option<Arc<AutoSave>> {
    if disabled {
        log::info!("Auto-save disabled (--no-auto-save)");
        return None;
    }
    let dir = match (flag, env) {
        (Some(d), _) => d.to_path_buf(),
        (None, Some(v)) => PathBuf::from(v.to_string_lossy().split(':').next().unwrap_or("")),
        (None, None) => {
            warn!(
                "Auto-save disabled: no state directory (StateDirectory= in the unit, \
                 or --state-dir)"
            );
            return None;
        }
    };
    if dir.as_os_str().is_empty() {
        warn!("Auto-save disabled: empty state directory");
        return None;
    }
    if let Err(e) = fs::create_dir_all(&dir) {
        warn!("Auto-save disabled: cannot create {}: {e}", dir.display());
        return None;
    }
    log::info!(
        "Auto-save on: a changed setting is saved to the card after {}s unchanged; records in {}",
        delay.as_secs(),
        dir.display()
    );
    Some(AutoSave::new(StateStore::new(dir), delay))
}

/// Schedules debounced saves for the gRPC service.
pub struct AutoSave {
    store: StateStore,
    delay: Duration,
    completed: watch::Sender<u64>,
}

impl AutoSave {
    pub fn new(store: StateStore, delay: Duration) -> Arc<Self> {
        Arc::new(AutoSave {
            store,
            delay,
            completed: watch::Sender::new(0),
        })
    }

    pub fn store(&self) -> &StateStore {
        &self.store
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Counts finished save attempts (any outcome). For tests and diagnostics.
    pub fn completed(&self) -> watch::Receiver<u64> {
        self.completed.subscribe()
    }

    /// Called after apply number `generation` succeeded. Every apply starts its own
    /// timer; only the one whose generation is still current when it fires acts, so
    /// the save happens `delay` after the last change.
    pub fn schedule(self: &Arc<Self>, dev: Arc<GpuDevice>, generation: u64) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(me.delay).await;
            me.run(dev, Some(generation)).await;
        });
    }

    /// Runs one compare-then-save attempt. `generation: None` means "whatever is
    /// pending now" (shutdown flush).
    pub async fn run(&self, dev: Arc<GpuDevice>, generation: Option<u64>) -> SaveOutcome {
        let store = self.store.clone();
        let id = dev.id.clone();
        let outcome = tokio::task::spawn_blocking(move || dev.try_save(&store, generation))
            .await
            .unwrap_or_else(|e| {
                warn!("{id}: save task failed: {e}");
                SaveOutcome::Failed
            });
        debug!("{id}: auto-save attempt: {outcome:?}");
        self.completed.send_modify(|n| *n += 1);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::PciIds;

    fn key(pci: &str, sd: u16) -> CardKey {
        CardKey {
            pci_address: pci.to_string(),
            ids: PciIds {
                vendor: 0x1002,
                device: 0x747e,
                subsystem_vendor: 0x1458,
                subsystem_device: sd,
            },
        }
    }

    const P: [Packet; 2] = [[0x88, 1, 2, 0x63, 0, 1, 0, 0], [0xB0, 1, 1, 2, 3, 1, 2, 3]];

    #[test]
    fn round_trip_and_keying() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::new(dir.path());
        let a = key("0000:04:00.0", 0x2414);
        assert_eq!(s.load(&a), None);
        s.store(&a, &P).unwrap();
        assert_eq!(s.load(&a), Some(P.to_vec()));
        // Another slot, or another card in the same slot, has its own record.
        assert_eq!(s.load(&key("0000:0a:00.0", 0x2414)), None);
        assert_eq!(s.load(&key("0000:04:00.0", 0x2415)), None);
        assert!(
            s.path_for(&a)
                .ends_with("0000:04:00.0_1002-747e-1458-2414.saved")
        );
        // No temp files left behind.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        s.clear(&a).unwrap();
        assert_eq!(s.load(&a), None);
        s.clear(&a).unwrap(); // clearing twice is fine
    }

    #[test]
    fn invalid_records_are_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::new(dir.path());
        let a = key("0000:04:00.0", 0x2414);
        for bad in [
            "",
            "garbage\n",
            "cc-gigabyte-gpu-rgb saved-state v1\n",
            "cc-gigabyte-gpu-rgb saved-state v1\n88 01 02\n",
            "cc-gigabyte-gpu-rgb saved-state v1\nzz 00 00 00 00 00 00 00\n",
            "cc-gigabyte-gpu-rgb saved-state v2\n88 01 02 63 00 01 00 00\n",
        ] {
            fs::write(s.path_for(&a), bad).unwrap();
            assert_eq!(s.load(&a), None, "{bad:?}");
        }
        // A directory where the file should be: unreadable, still unknown.
        fs::remove_file(s.path_for(&a)).unwrap();
        fs::create_dir(s.path_for(&a)).unwrap();
        assert_eq!(s.load(&a), None);
    }

    #[test]
    fn path_is_sanitised() {
        let s = StateStore::new("/state");
        let p = s.path_for(&key("../../etc/x", 1));
        assert_eq!(p.parent(), Some(Path::new("/state")));
    }

    #[test]
    fn cli_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let a = key("0000:04:00.0", 0x2414);
        // set --save records what it saved.
        assert!(cli_record_saved(dir.path(), &a, &P).unwrap());
        assert_eq!(StateStore::new(dir.path()).load(&a), Some(P.to_vec()));
        // plain save makes it unknown.
        assert!(cli_forget(dir.path(), &a).unwrap());
        assert_eq!(StateStore::new(dir.path()).load(&a), None);
        // Missing directory: nothing to update, not an error.
        let missing = dir.path().join("missing");
        assert!(!cli_record_saved(&missing, &a, &P).unwrap());
        assert!(!cli_forget(&missing, &a).unwrap());
        assert!(!missing.exists());
    }
}
