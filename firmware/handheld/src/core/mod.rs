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

static CORE_MANAGER: LazyLock<Mutex<CoreManager>> =
    LazyLock::new(|| Mutex::new(CoreManager::new()));

pub struct CoreManager {
    stage: Stage,
    core: Option<&'static CoreInfo>,
    selected_files: Vec<Option<PathBuf>>,
}

pub struct CoreInfo {
    id: &'static str,
    #[allow(unused)]
    name: &'static str,
    #[allow(unused)]
    author: &'static str,
    files: &'static [CoreFile],
}

pub struct CoreFile {
    label: &'static str,
    extensions: &'static [&'static str],
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Stage {
    Idle,
    LoadInit,
    LoadSelectFile(usize),
    LoadBitstream,
    Running,
}

static CORES: &[CoreInfo] = &[
    CoreInfo {
        id: "Game-Bub.GB",
        name: "Game Boy / Game Boy Color",
        author: "Game Bub",
        files: &[CoreFile {
            label: "ROM",
            extensions: &[".gb", ".gbc"],
        }],
    },
    CoreInfo {
        id: "Game-Bub.GBA",
        name: "Game Boy Advance",
        author: "Game Bub",
        files: &[CoreFile {
            label: "ROM",
            extensions: &[".gba"],
        }],
    },
];

impl CoreManager {
    fn new() -> Self {
        CoreManager {
            core: None,
            stage: Stage::Idle,
            selected_files: Vec::new(),
        }
    }

    pub fn lock() -> MutexGuard<'static, Self> {
        CORE_MANAGER.lock().unwrap()
    }

    /// Start the process of running a specific core (by ID).
    ///
    /// Overall process:
    ///  * Make N file select requests (and service directory changes)
    ///  * Move to loading screen
    ///  * Load the new bitstream
    ///  * Load all files
    ///  * Tell core to go
    pub fn run_core(&mut self, id: &str) {
        log::info!("Run core: {id}");
        assert!(self.core.is_none());
        assert!(self.stage == Stage::Idle);
        self.core = CORES.iter().find(|x| x.id == id);
        let Some(core) = self.core else {
            log::error!("Core not found: '{id}'");
            return;
        };
        self.stage = Stage::LoadInit;

        self.selected_files = vec![None; core.files.len()];
        self.next_file_select();
    }

    pub fn exit_core(&mut self) {
        self.core = None;
        self.stage = Stage::Idle;
        self.selected_files.clear();

        // Cut cartridge power (if enabled)
        Device::lock().set_cart_power(false);
        // And go back to the boot bitstream
        bitstream::current().ensure_boot().unwrap();
    }

    fn next_file_select(&mut self) {
        let core = self.core.unwrap();
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
            // TODO: if core.files[index] is not one that requires selection, continue to next.
            self.stage = Stage::LoadSelectFile(index);
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
        match self.core.unwrap().id {
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
        self.core = None;
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
        let extensions = self.core.unwrap().files[file_index].extensions;

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
