// SPDX-License-Identifier: GPL-3.0-or-later
//
// cc-gigabyte-gpu-rgb command line.
// Logging and signal handling follow the CoolerControl device-service template
// (GPL-3.0-or-later).

use anyhow::{Context, Result, anyhow, bail};
use cc_gigabyte_gpu_rgb::autosave::{self, DEFAULT_STATE_DIR};
use cc_gigabyte_gpu_rgb::bus::{self, LinuxI2cBus};
use cc_gigabyte_gpu_rgb::discovery::{self, DEFAULT_SYSFS_ROOT, FoundBus};
use cc_gigabyte_gpu_rgb::protocol::{self, Mode, Rgb, Speed};
use cc_gigabyte_gpu_rgb::service::GigabyteGpuService;
use cc_gigabyte_gpu_rgb::{DEFAULT_SOCKET, SERVICE_NAME, VERSION, detect_devices, server};
use clap::{Args as ClapArgs, Parser, Subcommand};
use log::{LevelFilter, info, warn};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};

/// Log level passed by the CoolerControl daemon to plugins it starts.
const ENV_CC_LOG: &str = "CC_LOG";
/// Set by systemd for `StateDirectory=`.
const ENV_STATE_DIRECTORY: &str = "STATE_DIRECTORY";
/// Upper bound for flushing a pending save at shutdown.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// CoolerControl device-service plugin for Gigabyte RGB Fusion 2 GPU lighting over I2C.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Debug logging, including every I2C transfer as hex
    #[arg(short, long, global = true)]
    debug: bool,

    /// sysfs mount point (for testing)
    #[arg(long, global = true, value_name = "DIR", default_value = DEFAULT_SYSFS_ROOT)]
    sysfs_root: PathBuf,

    /// Directory with the record of what is saved on each card. `serve` defaults to
    /// $STATE_DIRECTORY (set by systemd); `set` and `save` to /var/lib/cc-gigabyte-gpu-rgb
    #[arg(long, global = true, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the gRPC device service on a Unix socket (default)
    Serve(ServeArgs),
    /// Find the card's I2C bus, probe the controller and print its reply
    Probe,
    /// Apply one lighting setting directly, without CoolerControl
    Set {
        /// static, breathing, flashing, double_flash, gradient, color_cycle or off
        mode: String,
        /// Colour as RRGGBB (required for modes that take a colour)
        #[arg(long, short = 'c', alias = "colour", value_name = "RRGGBB")]
        color: Option<String>,
        /// slowest, slower, normal, faster or fastest
        #[arg(long, short = 's', default_value = "normal")]
        speed: String,
        /// Also store the setting in the card's memory afterwards, and record it
        #[arg(long)]
        save: bool,
    },
    /// Store the lighting the card shows now in its own memory, so it is shown from
    /// power-on and after resume. Always writes, and forgets the auto-save record
    Save,
}

#[derive(ClapArgs, Debug)]
struct ServeArgs {
    /// Unix socket path; must match `address` in manifest.toml
    #[arg(long, value_name = "PATH", default_value = DEFAULT_SOCKET)]
    socket: PathBuf,
    /// Save to the card once a changed setting has been unchanged this long
    #[arg(long, value_name = "SECONDS", default_value_t = autosave::DEFAULT_DELAY.as_secs())]
    auto_save_delay: u64,
    /// Never save to the card's memory automatically
    #[arg(long)]
    no_auto_save: bool,
}

impl Default for ServeArgs {
    fn default() -> Self {
        ServeArgs {
            socket: PathBuf::from(DEFAULT_SOCKET),
            auto_save_delay: autosave::DEFAULT_DELAY.as_secs(),
            no_auto_save: false,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    setup_logging(args.debug);
    let cli_state_dir = args
        .state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR));
    let result = match args.command {
        None => serve(
            &args.sysfs_root,
            args.state_dir.as_deref(),
            &ServeArgs::default(),
        ),
        Some(Command::Serve(ref s)) => serve(&args.sysfs_root, args.state_dir.as_deref(), s),
        Some(Command::Probe) => probe(&args.sysfs_root),
        Some(Command::Set {
            ref mode,
            ref color,
            ref speed,
            save,
        }) => set(
            &args.sysfs_root,
            &cli_state_dir,
            mode,
            color.as_deref(),
            speed,
            save,
        ),
        Some(Command::Save) => save(&args.sysfs_root, &cli_state_dir),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn serve(sysfs_root: &Path, state_dir: Option<&Path>, args: &ServeArgs) -> Result<()> {
    info!("Starting {SERVICE_NAME} v{VERSION}");
    let devices = detect_devices(sysfs_root);
    info!("Serving {} device(s)", devices.len());
    let socket = args.socket.as_path();
    let auto_save = autosave::configure(
        args.no_auto_save,
        state_dir,
        std::env::var_os(ENV_STATE_DIRECTORY).as_deref(),
        Duration::from_secs(args.auto_save_delay),
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start async runtime")?;
    rt.block_on(async {
        // Timers are spawned on this runtime, so the service is built inside it.
        let service = GigabyteGpuService::new(devices).with_auto_save(auto_save);
        let flusher = service.flusher();
        let shutdown = shutdown_signal()?;
        let result = server::serve_uds(service, socket, shutdown).await;
        flusher.flush(FLUSH_TIMEOUT).await;
        result
    })
}

/// Resolves on SIGTERM or SIGINT. Must be called inside the runtime.
fn shutdown_signal() -> Result<impl std::future::Future<Output = ()>> {
    let mut term = signal(SignalKind::terminate()).context("cannot install SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("cannot install SIGINT handler")?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => info!("SIGTERM received, shutting down"),
            _ = int.recv() => info!("SIGINT received, shutting down"),
        }
    })
}

/// Finds exactly one supported card and opens its bus.
fn open_single(sysfs_root: &Path) -> Result<(FoundBus, LinuxI2cBus)> {
    let mut found = discovery::discover_supported(sysfs_root)
        .with_context(|| format!("cannot scan {}/bus/i2c/devices", sysfs_root.display()))?;
    let f = match found.len() {
        0 => bail!("no supported card found (is the i2c-dev module loaded?)"),
        1 => found.remove(0),
        n => {
            // Use the first one, say so.
            log::warn!("{n} supported cards found; using the first");
            found.remove(0)
        }
    };
    println!(
        "{} at {}: bus i2c-{} '{}', address 0x{:02x}",
        f.card.name, f.pci_address, f.bus_number, f.bus_name, f.card.address
    );
    let path = f.dev_path();
    let bus = LinuxI2cBus::open(&path).map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
            " (run as root)"
        } else {
            ""
        };
        anyhow!("cannot open {}: {e}{hint}", path.display())
    })?;
    Ok((f, bus))
}

fn probe(sysfs_root: &Path) -> Result<()> {
    let (f, mut bus) = open_single(sysfs_root)?;
    match bus::probe(&mut bus, f.card.address) {
        Ok(reply) => {
            println!("probe reply: {} (ok)", protocol::hex(&reply));
            Ok(())
        }
        Err(e) => Err(anyhow!(e)),
    }
}

fn save(sysfs_root: &Path, state_dir: &Path) -> Result<()> {
    let packets = protocol::encode_save();
    let (f, mut bus) = open_single(sysfs_root)?;
    bus::probe(&mut bus, f.card.address)?;
    bus::write_packets(&mut bus, f.card.address, &packets).context("I2C write failed")?;
    println!("saved the current lighting to the card's memory");
    // What the card was showing is not known here, so the record becomes "unknown".
    match autosave::cli_forget(state_dir, &f.key()) {
        Ok(true) => info!("Cleared the auto-save record in {}", state_dir.display()),
        Ok(false) => {}
        Err(e) => warn!(
            "Cannot clear the auto-save record in {}: {e}",
            state_dir.display()
        ),
    }
    Ok(())
}

fn set(
    sysfs_root: &Path,
    state_dir: &Path,
    mode: &str,
    color: Option<&str>,
    speed: &str,
    save: bool,
) -> Result<()> {
    let mode = Mode::from_name(mode).ok_or_else(|| {
        let names: Vec<&str> = Mode::ALL.iter().map(|m| m.name()).collect();
        anyhow!("unknown mode '{mode}'; one of: {}", names.join(", "))
    })?;
    let speed = Speed::from_name(speed).ok_or_else(|| {
        let names: Vec<&str> = Speed::ALL.iter().map(|s| s.name()).collect();
        anyhow!("unknown speed '{speed}'; one of: {}", names.join(", "))
    })?;
    let color = color
        .map(|c| Rgb::parse_hex(c).ok_or_else(|| anyhow!("invalid colour '{c}', expected RRGGBB")))
        .transpose()?;
    // Encode before touching the bus so that bad arguments never cause I/O.
    let lighting = protocol::encode(mode, speed, color)?;
    let mut packets = lighting.clone();
    if save {
        packets.extend(protocol::encode_save());
    }

    let (f, mut bus) = open_single(sysfs_root)?;
    bus::probe(&mut bus, f.card.address)?;
    bus::write_packets(&mut bus, f.card.address, &packets).context("I2C write failed")?;
    let saved = if save {
        " and saved it to the card"
    } else {
        ""
    };
    println!("applied {mode}{saved} ({} packets)", packets.len());
    if save {
        match autosave::cli_record_saved(state_dir, &f.key(), &lighting) {
            Ok(true) => info!("Recorded the saved setting in {}", state_dir.display()),
            Ok(false) => {}
            Err(e) => {
                warn!(
                    "Cannot record the saved setting in {}: {e}",
                    state_dir.display()
                );
                // Do not leave a stale belief behind.
                let _ = autosave::cli_forget(state_dir, &f.key());
            }
        }
    }
    Ok(())
}

/// `--debug` wins; otherwise `CC_LOG` (as set by CoolerControl) or `RUST_LOG`
/// style levels; default info. Under journald no timestamps are added.
fn setup_logging(debug: bool) {
    let level = if debug {
        LevelFilter::Debug
    } else {
        std::env::var(ENV_CC_LOG)
            .ok()
            .and_then(|l| LevelFilter::from_str(&l).ok())
            .unwrap_or(LevelFilter::Info)
    };
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(LevelFilter::Warn)
        .filter_module("cc_gigabyte_gpu_rgb", level);
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        builder.format_timestamp(None).format_target(false);
    }
    builder.init();
}
