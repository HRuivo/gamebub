use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
};

use crate::ui;

static CORE_MANAGER: LazyLock<Mutex<CoreManager>> =
    LazyLock::new(|| Mutex::new(CoreManager::new()));

pub struct CoreManager {
    core_id: Option<String>,
}

impl CoreManager {
    fn new() -> Self {
        CoreManager { core_id: None }
    }

    pub fn lock() -> MutexGuard<'static, Self> {
        CORE_MANAGER.lock().unwrap()
    }

    /// Start the process of running a specific core (by ID).
    pub fn run_core(&mut self, id: &str) {
        assert!(self.core_id.is_none());
        log::info!("Run core: {id}");
        self.core_id = Some(id.to_string());

        // Overall process:
        // * Make N file select requests (and service directory changes)
        // * Move to loading screen
        // * Load core's bitstream (turn display off, load, poll, turn display on)
        // * Load all assets as needed...
        // * Tell core to go

        // TODO: use correct label
        // TODO: use correct starting directory
        ui::send(ui::Message::CoreFileSelectBegin {
            label: "ROM".to_string(),
            path: "/sdcard".into(),
        });
        self.send_core_file_list(Path::new("/sdcard"));
    }

    /// Called when a file has been selected for the core.
    /// This could be a file or a directory.
    pub fn handle_file_selected(&mut self, path: PathBuf) {
        if path.is_file() {
            log::info!("File selected: {}", path.display());
            // TODO
        } else if path.is_dir() {
            self.send_core_file_list(&path);
        }
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
        let extensions: &[_] = match self.core_id.as_ref().unwrap().as_str() {
            "Game-Bub.GB" => &[".gb", ".gbc"],
            "Game-Bub.GBA" => &[".gba"],
            _ => &[],
        };

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
}
