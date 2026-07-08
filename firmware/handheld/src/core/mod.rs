use std::{
    ops::DerefMut,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
};

use crate::{
    bitstream::{self, CurrentBitstream},
    device::Device,
    ui,
};

mod info;

static CORE_MANAGER: LazyLock<Mutex<CoreManager>> =
    LazyLock::new(|| Mutex::new(CoreManager::new()));

pub struct CoreManager {
    stage: Stage,
    core_info: Option<&'static info::CoreInfo>,
    selected_files: Vec<Option<PathBuf>>,
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
trait CoreHandler {}

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
            selected_files: Vec::new(),
        }
    }

    pub fn lock() -> MutexGuard<'static, Self> {
        CORE_MANAGER.lock().unwrap()
    }

    fn get_core_handler(&mut self) -> Option<&mut dyn CoreHandler> {
        None
    }

    /// Start the process of running a specific core (by ID).
    pub fn run_core(&mut self, id: &str) {
        log::info!("Run core: {id}");
        assert!(self.core_info.is_none());
        assert!(self.stage == Stage::Idle);
        self.core_info = info::get_core_info(id);
        let Some(core) = self.core_info else {
            log::error!("Core not found: '{id}'");
            return;
        };
        self.stage = Stage::LoadInit;

        self.selected_files = vec![None; core.files.len()];
        self.next_file_select();
    }

    pub fn exit_core(&mut self) {
        self.core_info = None;
        self.stage = Stage::Idle;
        self.selected_files.clear();

        // Cut cartridge power (if enabled)
        Device::lock().set_cart_power(false);
        // And go back to the boot bitstream
        bitstream::current().ensure_boot().unwrap();
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
            if !core.files[index].user_selected {
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

        // TODO generalize
        match self.core_info.unwrap().id {
            "Game-Bub.GB" => bitstream::current().ensure_gameboy().unwrap(),
            "Game-Bub.GBA" => bitstream::current().ensure_gba().unwrap(),
            _ => panic!(),
        };

        // TODO: enter core loading screen
        ui::send(ui::Message::EnterGame);

        // TODO generalize
        let rom_path = self.selected_files[0].take().unwrap();
        let result: Result<(), String> = match bitstream::current().deref_mut() {
            CurrentBitstream::None => Err("no bitstream".into()),
            CurrentBitstream::Gameboy(x) => x
                .set_emulated_cartridge(rom_path.as_path())
                .map_err(|e| e.to_string()),
            CurrentBitstream::Gba(x) => x
                .set_emulated_cartridge(rom_path.as_path())
                .map_err(|e| e.to_string()),
        };

        match result {
            Ok(()) => {
                // Clear loading bar
                ui::send(ui::Message::EnterGame);
                self.stage = Stage::Running;
            }
            Err(err) => {
                bitstream::current().ensure_boot().unwrap();
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
