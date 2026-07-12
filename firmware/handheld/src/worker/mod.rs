//! Worker threads to do background blocking work.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, OnceLock};

use crate::core::CoreManager;
use crate::device::drivers::fpga;
use crate::device::Device;
use crate::device::DisplayMode;
use crate::fwinfo::FirmwareVersion;
use crate::input::InputManager;
use crate::{kvs, ui};

#[derive(Debug)]
pub enum Message {
    /// An interrupt request from the FPGA
    FpgaIrq(u32),
    /// The headphone state has changed
    HeadphoneState(bool),
    /// The handheld has docked
    DockBegin {
        serial: u32,
        #[allow(unused)]
        hardware: u32,
        firmware: u32,
    },
    /// The handheld has undocked
    DockEnd,

    /// Run a cartridge
    RunCartridge,
    /// Run a ROM file
    RunRomFile(#[allow(unused)] PathBuf),
    /// Load ROM select entries
    ListRoms(PathBuf),
    /// The idle timer has expired
    IdleTimerExpired,

    /// Start running a core (ID)
    RunCore(String),
    /// Exit the current core (or cancel loading)
    ExitCore,
    /// A file was selected for the core (could be a directory).
    CoreFileSelected(PathBuf),
    /// Core file selection was cancelled.
    CoreFileCancelled,
    /// Core focus changed
    CoreFocusChanged(bool),
}

/// Send a message to the worker threads.
pub fn send(message: Message) {
    match SENDER.get() {
        Some(sender) => sender.send(message).unwrap(),
        None => log::error!("Dropping worker message {:?}", message),
    }
}

/// Start the worker threadpool. Called once during system init. Panics if called twice.
pub fn start() {
    let (sender, receiver) = mpsc::channel::<Message>();
    SENDER.set(sender).expect("Worker already initialized");

    // TODO: look into reducing stack usage
    std::thread::Builder::new()
        .name("Worker".to_string())
        .stack_size(16 * 1024)
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                log::debug!("Dispatch {:?}", message);
                dispatch(message);
            }
        })
        .unwrap();
}

static SENDER: OnceLock<mpsc::Sender<Message>> = OnceLock::new();

fn dispatch(message: Message) {
    match message {
        Message::FpgaIrq(irq_mask) => {
            if (irq_mask & fpga::Irq::ModuleVblank.as_flag()) != 0 {
                // Module vblank
                if let Some(bitstream) = CoreManager::lock().current_bitstream() {
                    bitstream.on_vblank_irq();
                }
            }
        }
        Message::HeadphoneState(has_headphones) => {
            log::info!("Headphone detection: {}", has_headphones);
            let mut device = Device::lock();
            device.dac.set_headphones_enabled(has_headphones).unwrap();
            device.dac.set_speakers_enabled(!has_headphones).unwrap();
        }
        Message::RunCartridge => {
            let cart_type = {
                let mut device = Device::lock();
                device.get_cart_switch()
            };
            log::info!("Cart switch: {}", cart_type);

            let core_id = if cart_type {
                "Game-Bub.GB"
            } else {
                "Game-Bub.GBA"
            };

            CoreManager::lock().run_core(core_id, true);
        }
        Message::RunRomFile(_) => {
            // TODO: remove
        }
        Message::ListRoms(path) => {
            let files = match rom_select_get_files(&path) {
                Ok(files) => files,
                Err(e) => {
                    log::warn!("Error listing directory: {:?}", e);
                    ui::send(ui::Message::RomSelectError(format!(
                        "Error listing directory:\n{}",
                        e,
                    )));
                    Vec::new()
                }
            };
            ui::send(ui::Message::RomSelectFiles(files))
        }
        Message::DockBegin {
            serial, firmware, ..
        } => {
            ui::send(ui::Message::DockBegin {
                serial: format!("{serial:08X}"),
                firmware: format!("{}", FirmwareVersion::from(firmware)),
            });

            InputManager::lock().remove_all_gamepads();
            let mut device = Device::lock();
            device.docked = true;
            device.change_display_mode(DisplayMode::External).unwrap();
        }
        Message::DockEnd => {
            ui::send(ui::Message::DockEnd);
            InputManager::lock().remove_all_gamepads();
            let mut device = Device::lock();
            device.docked = false;
            device.change_display_mode(DisplayMode::Internal).unwrap();
        }
        Message::IdleTimerExpired => {
            // If the idle timer expires during setup, just power off.
            let setup_stage = kvs::keys::SETUP_STAGE.get().unwrap_or_default();
            if setup_stage == 0 {
                log::warn!("Idle during setup, powering off.");
                Device::lock().power_off();
            }
            // TODO: Dim the screen temporarily.
        }
        Message::RunCore(id) => CoreManager::lock().run_core(&id, false),
        Message::ExitCore => CoreManager::lock().exit_core(),
        Message::CoreFileSelected(file) => CoreManager::lock().handle_file_selected(file),
        Message::CoreFileCancelled => CoreManager::lock().cancel_file_select(),
        Message::CoreFocusChanged(focused) => CoreManager::lock().focus_changed(focused),
        #[allow(unreachable_patterns)]
        _ => {
            log::warn!("Unhandled message: {:?}", message);
        }
    }
}

/// Get the list of eligible files for the ROM select menu at the given directory
fn rom_select_get_files(path: &Path) -> std::io::Result<Vec<(String, bool)>> {
    let mut files = path
        .read_dir()?
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name();
            let name = name.to_str()?;
            let kind = e.metadata().ok()?.file_type();
            if name.starts_with(".") {
                return None;
            }
            let extensions = &[".gb", ".gbc", ".gba"];
            if kind.is_file() && !extensions.iter().any(|&ext| name.ends_with(ext)) {
                return None;
            }
            Some((name.to_string(), kind))
        })
        .collect::<Vec<_>>();
    files.sort_unstable_by(|f1, f2| {
        // Sort by name, with directories first.
        let c1 = (f1.1.is_file(), f1.0.as_str());
        let c2 = (f2.1.is_file(), f2.0.as_str());
        c1.cmp(&c2)
    });
    let files = files.into_iter().map(|f| (f.0, f.1.is_dir())).collect();
    Ok(files)
}
