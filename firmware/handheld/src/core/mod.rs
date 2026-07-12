use std::{
    fs::File,
    io::Write,
    ops::DerefMut as _,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use esp_idf_svc::hal::units::Hertz;
use thiserror::Error;

use crate::{
    bitstream,
    core::CoreError::*,
    device::{
        drivers::fpga::{SpiCommand, MAX_SPI_READ_CLOCK},
        Device,
    },
    ui,
};

mod info;

const PROGRESS_UPDATE_INTERVAL: Duration = Duration::from_millis(250);

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

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Missing path for file {0}")]
    MissingRequiredFile(String),
    #[error("Cannot open file {0}")]
    CannotOpenFile(String),
    #[error("Failed to load file {0}:\n{1}")]
    FailedLoadFile(String, String),
    #[error("Failed to save file {0}:\n{1}")]
    FailedSaveFile(String, String),
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

    /// Called before loading a file, returns the path override.
    fn get_file_path_override(&mut self, id: u16) -> Option<PathBuf>;

    /// Called before a file is loaded, for any additional stuff.
    fn on_before_file_load(&mut self, id: u16, file: &mut File) -> Result<(), String>;

    /// Called for each chunk of a file loaded
    fn on_during_file_load(&mut self, _id: u16, _data: &[u8]) {}

    /// Called after a file is loaded, for any additional stuff.
    fn on_after_file_load(&mut self, _id: u16) {}

    fn on_before_run(&mut self) -> Result<(), String>;

    fn on_focus_changed(&mut self, has_focus: bool);

    /// Called before saving a file, returns the size of the file.
    fn get_file_size(&mut self, id: u16) -> u32;

    /// Called after file is written, to write any additional data.
    fn on_after_file_save(&mut self, id: u16, file: &mut File) -> Result<(), String>;

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

    pub fn prepare_for_power_off(&mut self) {
        if self.stage == Stage::Running {
            if let Err(e) = self.persist_files() {
                log::error!("Error saving: {}", e);
            }
        }
        self.reset_state();
    }

    pub fn exit_core(&mut self) {
        if let Err(e) = self.persist_files() {
            log::error!("Error saving: {}", e);
        }

        self.reset_state();

        // And go back to the boot bitstream
        bitstream::program_boot();
    }

    fn reset_state(&mut self) {
        self.core_info = None;
        self.core_handler = CoreHandlerImpl::None;
        self.stage = Stage::Idle;
        self.selected_files.clear();

        Device::lock().set_cart_power(false);
    }

    pub fn focus_changed(&mut self, has_focus: bool) {
        self.get_core_handler().unwrap().on_focus_changed(has_focus);
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
                self.finish_loading();
                return;
            }
            self.stage = Stage::LoadSelectFile(index);
            let file = &core.files[index];
            if !file.user_selected {
                continue;
            }
            if self.run_cartridge && (file.id == 0 || file.dependent_on_0) {
                // TODO: validate and make sure there's a file 0
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

    fn finish_loading(&mut self) {
        self.load_bitstream();

        if self.run_cartridge {
            let mut device = Device::lock();
            device.set_cart_power(true);
        }

        if let Err(e) = self.load_files() {
            self.reset_state();
            bitstream::program_boot();
            ui::send(ui::Message::CoreLoadError(e.to_string()));
            return;
        }

        // TODO: replace with generic loading sequence

        let result = self.get_core_handler().unwrap().on_before_run();
        match result {
            Ok(()) => {
                // Clear loading bar
                ui::send(ui::Message::EnterGame);
                self.stage = Stage::Running;
            }
            Err(err) => {
                self.reset_state();
                bitstream::program_boot();
                ui::send(ui::Message::CoreLoadError(err.to_string()));
                return;
            }
        }
    }

    fn load_bitstream(&mut self) {
        assert!(self.stage == Stage::LoadBitstream);

        let bitstream_path = self.get_core_handler().unwrap().get_bitstream_path();
        bitstream::program_fpga(&bitstream_path);
        self.get_core_handler().unwrap().on_after_program();

        // TODO: enter core loading screen
        ui::send(ui::Message::EnterGame);
    }

    fn load_files(&mut self) -> Result<(), CoreError> {
        // TODO: Sum of size of files to load (rather than doing it file-by-file).
        let mut overall_transferred = 0u64;
        let mut overall_total = 0u64;
        let mut last_progress_update = Instant::now();

        let mut scratch = crate::bitstream::SCRATCH.take().expect("scratch buffer");

        let core = self.core_info.unwrap();
        let file_0_index = core.files.iter().position(|f| f.id == 0);
        for (i, info) in core.files.iter().enumerate() {
            if self.run_cartridge && (info.id == 0 || info.dependent_on_0) {
                continue;
            }

            // Get the file path
            let mut path = self.selected_files[i].clone();

            // TODO if asset path is not none, construct path based on that

            if info.dependent_on_0 {
                // Construct a new path based on file 0's path
                assert!(path.is_none());
                let extension = &info.extensions[0][1..]; // Remove the dot
                let file_0_index = file_0_index.unwrap();
                path = self.selected_files[file_0_index]
                    .as_ref()
                    .map(|p| p.with_extension(extension));
            }

            // Possibly override the path
            path = path.or(self
                .get_core_handler()
                .unwrap()
                .get_file_path_override(info.id));

            let Some(path) = path else {
                if !info.optional {
                    return Err(MissingRequiredFile(info.label.to_string()));
                }
                // TODO: implement (optional) clear for empty file
                continue;
            };

            log::info!("Load file {} from {}", info.label, path.display());
            let mut file = match File::open(&path) {
                Ok(file) => file,
                Err(_) if info.optional && info.initialize => {
                    let buf = scratch.deref_mut();
                    buf.fill(0xFF);
                    let mut pos = 0u32;
                    let len = info.max_size.max(info.exact_size);
                    while pos < len {
                        let n = ((len - pos) as usize).min(buf.len());
                        let max_clock = Some(Hertz(info.max_transfer_speed * 1000 * 2));
                        let command = SpiCommand {
                            word_size: info.transfer_word_size,
                            byte_swap: true,
                            increment_address: true,
                        };
                        let _ = Device::lock().fpga.spi_write(
                            max_clock,
                            command,
                            info.address + pos,
                            &buf[..n],
                        );
                        pos += n as u32;
                    }
                    log::info!("Failed to open file, clearing");
                    continue;
                }
                Err(_) if info.optional => {
                    log::info!("Failed to open file, skipping");
                    continue;
                }
                Err(_) => {
                    return Err(CannotOpenFile(info.label.to_string()));
                }
            };

            self.selected_files[i] = Some(path);
            self.get_core_handler()
                .unwrap()
                .on_before_file_load(info.id, &mut file)
                .map_err(|e| FailedLoadFile(info.label.to_string(), e))?;
            let file_size = file.metadata().unwrap().len();
            overall_total += file_size;
            // TODO check max size and exact size

            let start_time = Instant::now();
            let mut transfer_duration = Duration::ZERO;
            let mut handler_duration = Duration::ZERO;
            let mut transferred = 0;
            // TODO: maybe only bother with background I/O for a large file (> 256KB?)
            let result = crate::util::background_io::iter_chunks(file, &mut scratch, |chunk| {
                let transfer_start = Instant::now();
                let max_clock = Some(Hertz(info.max_transfer_speed * 1000 * 2));
                let command = SpiCommand {
                    word_size: info.transfer_word_size,
                    byte_swap: true,
                    increment_address: true,
                };
                Device::lock()
                    .fpga
                    .spi_write(max_clock, command, info.address + transferred, chunk)
                    .unwrap();
                transferred += chunk.len() as u32;
                overall_transferred += chunk.len() as u64;
                transfer_duration += transfer_start.elapsed();

                let handler_start = Instant::now();
                self.get_core_handler()
                    .unwrap()
                    .on_during_file_load(info.id, chunk);
                handler_duration += handler_start.elapsed();

                // Update UI progress bar.
                if last_progress_update.elapsed() > PROGRESS_UPDATE_INTERVAL {
                    let progress = (overall_transferred as f32) / (overall_total as f32);
                    ui::send(ui::Message::RomLoadingProgress(progress));
                    last_progress_update = Instant::now();
                }
            });
            let read_duration = result
                .map_err(|_| FailedLoadFile(info.label.to_string(), "I/O error".to_string()))?;

            let duration = start_time.elapsed();
            self.get_core_handler().unwrap().on_after_file_load(info.id);
            log::info!(
                "Loaded {} bytes in {} ms ({}/{}/{} ms read/transfer/handler)",
                transferred,
                duration.as_millis(),
                read_duration.as_millis(),
                transfer_duration.as_millis(),
                handler_duration.as_millis(),
            );
        }

        Ok(())
    }

    fn persist_files(&mut self) -> Result<(), CoreError> {
        assert!(self.stage == Stage::Running);
        let core = self.core_info.unwrap();
        for (i, info) in core.files.iter().enumerate() {
            if info.read_only {
                continue;
            }
            if self.run_cartridge && (info.id == 0 || info.dependent_on_0) {
                continue;
            }

            let path = self.selected_files[i].clone().unwrap();
            log::info!("Saving file {} to {}", info.label, path.display());
            let size = self.get_core_handler().unwrap().get_file_size(info.id);

            let mut file = File::create(path)
                .map_err(|_| FailedSaveFile(info.label.to_string(), "Open failed".to_string()))?;
            let mut scratch = crate::bitstream::SCRATCH.take().expect("scratch buffer");
            let buf = scratch.deref_mut();
            let mut address: u32 = info.address;
            let mut bytes_left = size as usize;

            let start_time = Instant::now();
            while bytes_left > 0 {
                let to_read = bytes_left.min(buf.len());
                let data = &mut buf[0..to_read];

                let max_clock =
                    Some(Hertz(info.max_transfer_speed * 1000 * 2).min(MAX_SPI_READ_CLOCK));
                let command = SpiCommand {
                    word_size: info.transfer_word_size,
                    byte_swap: true,
                    increment_address: true,
                };
                let _ = Device::lock()
                    .fpga
                    .spi_read(max_clock, command, address, data);
                file.write(data).map_err(|_| {
                    FailedSaveFile(info.label.to_string(), "Write failed".to_string())
                })?;
                address += to_read as u32;
                bytes_left -= to_read;
            }
            log::info!(
                "Saved {} bytes in {}ms",
                size,
                start_time.elapsed().as_millis() as u32
            );

            self.get_core_handler()
                .unwrap()
                .on_after_file_save(info.id, &mut file)
                .map_err(|e| FailedSaveFile(info.label.to_string(), e))?;
        }
        Ok(())
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
