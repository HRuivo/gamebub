use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
};

use crate::{bitstream, device::Device, ui};

mod info;

static CORE_MANAGER: LazyLock<Mutex<CoreManager>> =
    LazyLock::new(|| Mutex::new(CoreManager::new()));

pub struct CoreManager {
    stage: Stage,
    core_info: Option<&'static info::CoreInfo>,
    core_handler: CoreHandlerImpl,
    selected_files: Vec<Option<PathBuf>>,
    run_cartridge: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Stage {
    Idle,
    LoadInit,
    LoadSelectFile(usize),
    LoadBitstream,
    Running,
}

/// Core-specific lifecycle callbacks.
///
/// The main purpose is to add core-specific customizations for built-in cores
/// while generic core functionality is being built.
pub trait CoreHandler {
    /// Returns the path to the bitstream.
    fn get_bitstream_path(&self) -> PathBuf;

    /// Called after the bitstream is programmed.
    fn on_after_program(&mut self);

    /// Temporary: Run Cartridge
    fn start_physical_cartridge(&mut self) -> Result<(), String>;

    /// Temporary: Run Rom
    fn start_emulated_cartridge(&mut self, rom: &Path) -> Result<(), String>;

    /// Temporary: as Bitstream trait
    fn as_legacy_bitstream(&mut self) -> &mut dyn bitstream::Bitstream;
}

enum CoreHandlerImpl {
    None,
    Gameboy(crate::bitstream::gameboy::Gameboy),
    Gba(crate::bitstream::gba::Gba),
}

/// # CoreManager
///
/// Manages the lifecycle of cores.
///
/// ### Load
///  * Stage: LoadInit
///    * Make N file select requests (and service directory changes)
///  * Stage: LoadBitstream
///  * Call: on_before_program (returns bitstream)
///  * ... load the bitstream
///  * Call: on_after_program
///  * Set cartridge power (if needed)
///  * For each file (N):
///    * Call: on_before_file_load (returns path)
///    ... load the file, calling on_peek_file_load ...
///    * Call: on_after_file_load
///  * Call: on_before_run
///  * Tell core to go (??)
///
/// ### Menu open or close (pause / unpause)
///  * Call: on_focus_changed
///  * Tell core (??)
///
/// ### Stop
///  * Call: on_before_stop
///  * Tell core (??)
///  * For each file (N):
///    ... skip if file not persistent or is read only ...
///    * Call: on_before_file_save
///    ... read and save the file to disk ...
///    * Call: on_after_file_save
///  * Cut cartridge power
///  * Reload boot bitstream
impl CoreManager {
    fn new() -> Self {
        CoreManager {
            core_info: None,
            stage: Stage::Idle,
            core_handler: CoreHandlerImpl::None,
            selected_files: Vec::new(),
            run_cartridge: false,
        }
    }

    pub fn lock() -> MutexGuard<'static, Self> {
        CORE_MANAGER.lock().unwrap()
    }

    fn get_core_handler(&mut self) -> Option<&mut dyn CoreHandler> {
        match &mut self.core_handler {
            CoreHandlerImpl::None => None,
            CoreHandlerImpl::Gameboy(gameboy) => Some(gameboy),
            CoreHandlerImpl::Gba(gba) => Some(gba),
        }
    }

    /// Start the process of running a specific core (by ID).
    pub fn run_core(&mut self, id: &str, run_cartridge: bool) {
        log::info!("Run core={id} cart={run_cartridge}");
        assert!(self.core_info.is_none());
        assert!(self.stage == Stage::Idle);
        self.core_info = info::get_core_info(id);
        let Some(core) = self.core_info else {
            log::error!("Core not found: '{id}'");
            return;
        };
        self.core_handler = match self.core_info.unwrap().id {
            "Game-Bub.GB" => CoreHandlerImpl::Gameboy(crate::bitstream::gameboy::Gameboy::new()),
            "Game-Bub.GBA" => CoreHandlerImpl::Gba(crate::bitstream::gba::Gba::new()),
            _ => CoreHandlerImpl::None,
        };

        self.stage = Stage::LoadInit;
        self.run_cartridge = run_cartridge;

        self.selected_files = vec![None; core.files.len()];
        self.next_file_select();
    }

    /// Temporary transitional method
    /// TODO: remove
    pub fn current_bitstream(&mut self) -> Option<&mut dyn bitstream::Bitstream> {
        self.get_core_handler().map(|c| c.as_legacy_bitstream())
    }

    pub fn exit_core(&mut self) {
        // TODO persist files

        self.core_info = None;
        self.core_handler = CoreHandlerImpl::None;
        self.stage = Stage::Idle;
        self.selected_files.clear();

        // Cut cartridge power (if enabled)
        Device::lock().set_cart_power(false);
        // And go back to the boot bitstream
        bitstream::program_boot();
    }

    fn next_file_select(&mut self) {
        let core = self.core_info.unwrap();
        let index = loop {
            // Find the index of the next file to load.
            let index = match self.stage {
                Stage::LoadInit => 0,
                Stage::LoadSelectFile(i) => i + 1,
                _ => panic!(),
            };
            if index >= core.files.len() {
                log::info!("File selection complete");
                self.stage = Stage::LoadBitstream;
                self.load_bitstream();
                return;
            }
            self.stage = Stage::LoadSelectFile(index);
            let file = &core.files[index];
            if !file.user_selected {
                continue;
            }
            if self.run_cartridge && (file.id == 0 || file.dependent_on_0) {
                continue;
            }
            break index;
        };

        // TODO: use correct starting directory
        let path = Path::new("/sdcard");
        let file = &core.files[index];

        ui::send(ui::Message::CoreFileSelectBegin {
            label: file.label.to_string(),
            path: path.into(),
        });
        self.send_core_file_list(path);
    }

    fn load_bitstream(&mut self) {
        assert!(self.stage == Stage::LoadBitstream);

        let bitstream_path = self.get_core_handler().unwrap().get_bitstream_path();
        bitstream::program_fpga(&bitstream_path);
        self.get_core_handler().unwrap().on_after_program();

        // Enable cartridge power after the bitstream is loaded.
        if self.run_cartridge {
            let mut device = Device::lock();
            device.set_cart_power(true);
        }

        // TODO: enter core loading screen
        ui::send(ui::Message::EnterGame);

        // TODO: replace with generic loading sequence
        let result = if self.run_cartridge {
            self.get_core_handler().unwrap().start_physical_cartridge()
        } else {
            let rom_path = self.selected_files[0].take().unwrap();
            self.get_core_handler()
                .unwrap()
                .start_emulated_cartridge(rom_path.as_path())
        };

        match result {
            Ok(()) => {
                // Clear loading bar
                ui::send(ui::Message::EnterGame);
                self.stage = Stage::Running;
            }
            Err(err) => {
                bitstream::program_boot();
                ui::send(ui::Message::RomSelectError(err))
            }
        }
    }

    /// Called when a file has been selected for the core.
    /// This could be a file or a directory.
    pub fn handle_file_selected(&mut self, path: PathBuf) {
        if path.is_file() {
            log::info!("File selected: {}", path.display());
            let Stage::LoadSelectFile(file_index) = self.stage else {
                panic!()
            };
            self.selected_files[file_index] = Some(path);
            self.next_file_select();
        } else if path.is_dir() {
            self.send_core_file_list(&path);
        }
    }

    /// Called if a file selection is cancelled.
    pub fn cancel_file_select(&mut self) {
        // TODO support multiple file select (go back to previous file)
        log::info!("File select cancelled");
        self.core_info = None;
        self.stage = Stage::Idle;
    }

    fn send_core_file_list(&mut self, path: &Path) {
        let files = match self.list_core_files(&path) {
            Ok(files) => files,
            Err(e) => {
                log::warn!("Error listing directory: {:?}", e);
                ui::send(ui::Message::CoreFileSelectError(format!(
                    "Error listing directory:\n{}",
                    e,
                )));
                Vec::new()
            }
        };
        ui::send(ui::Message::CoreFileSelectList(files))
    }

    /// Get the list of eligible files for the file select menu at the given directory
    fn list_core_files(&mut self, path: &Path) -> std::io::Result<Vec<(String, bool)>> {
        // Assumes we're in a valid file selection stage.
        let file_index = match self.stage {
            Stage::LoadSelectFile(i) => i,
            _ => panic!(),
        };
        let extensions = self.core_info.unwrap().files[file_index].extensions;

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
                if kind.is_file() && !extensions.iter().any(|&e| name.ends_with(e)) {
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
}
