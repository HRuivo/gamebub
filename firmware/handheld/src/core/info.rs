use arrayvec::ArrayString;
use serde::Deserialize;
use std::{fs::File, io::BufReader, path::PathBuf};

use crate::device::drivers::fpga;

pub const DIR_CORES: &str = "/sdcard/cores/";

#[derive(Deserialize)]
pub struct CoreListEntry {
    pub id: ArrayString<32>,
    pub name: ArrayString<32>,
    pub author: ArrayString<32>,
}

#[allow(unused)]
pub struct CoreInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub author: &'static str,
    pub files: &'static [CoreFile],
}

#[allow(unused)]
pub struct CoreFile {
    pub id: u16,
    pub label: &'static str,
    pub extensions: &'static [&'static str],

    /// If set, the file will be loaded from this path relative to the asset path.
    pub asset_path: Option<&'static str>,

    /// If true, the core will still run if the file is not loaded.
    pub optional: bool,
    /// If true, file will be loaded at core start and saved at core end.
    // persistent: bool,
    /// If true, file is treated as read-only, won't be saved at core end.
    pub read_only: bool,
    /// If true, the file path is selected by the user (filtered by extensions).
    pub user_selected: bool,
    /// If true, dependent on the file with ID 0 (and the path is determined based on that path + this extension).
    pub dependent_on_0: bool,
    /// If true, if the file is not loaded, the region will still be initialized with 0xFFs.
    pub initialize: bool,

    // TODO: maybe broad types based on how the user accesses them?
    /// The address to load the file to.
    pub address: u32,
    /// Maximum size of the file.
    pub max_size: u32,
    /// Exact size of the file.
    pub exact_size: u32,
    /// Maximum read/write speed when loading/saving the file (in KB/s)
    pub max_transfer_speed: u32,

    /// Word size during transfer
    /// TODO: remove this, make all transfers 32-bit
    pub transfer_word_size: fpga::FpgaSpiWordSize,
}

impl CoreInfo {
    pub fn get_settings_path(&self) -> PathBuf {
        let mut p = PathBuf::from(super::DIR_SETTINGS);
        p.push(self.id);
        p.add_extension("json");
        p
    }
}

static CORES: &[CoreInfo] = &[
    CoreInfo {
        id: "Game-Bub.GB",
        name: "Game Boy / Game Boy Color",
        author: "Game Bub",
        files: &[
            CoreFile {
                id: 0,
                label: "ROM",
                extensions: &[".gb", ".gbc"],
                asset_path: None,

                optional: true,
                read_only: true,
                user_selected: true,
                dependent_on_0: false,
                initialize: false,

                address: 0x3000_0000, // SDRAM
                max_size: 8 * 1024 * 1024,
                exact_size: 0,
                max_transfer_speed: 10_000, // 10 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits32,
            },
            CoreFile {
                id: 1,
                label: "Save",
                extensions: &[".sav"],
                asset_path: None,

                optional: true,
                read_only: false,
                user_selected: false,
                dependent_on_0: true,
                initialize: true,

                address: 0x4000_0000, // SRAM
                max_size: 128 * 1024 + 48,
                exact_size: 0,
                max_transfer_speed: 5_000, // 5 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits16,
            },
            CoreFile {
                id: 2,
                label: "BIOS CGB",
                extensions: &[".bin"],
                asset_path: None, // TODO

                optional: false,
                read_only: true,
                user_selected: false,
                dependent_on_0: false,
                initialize: false,

                address: 0x1000_0000 + 256,
                max_size: 0,
                exact_size: 2048 + 256,
                max_transfer_speed: 5_000, // 5 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits8,
            },
            CoreFile {
                id: 3,
                label: "BIOS DMG",
                extensions: &[".bin"],
                asset_path: None, // TODO

                optional: false,
                read_only: true,
                user_selected: false,
                dependent_on_0: false,
                initialize: false,

                address: 0x1000_0000,
                max_size: 0,
                exact_size: 256,
                max_transfer_speed: 5_000, // 5 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits8,
            },
        ],
    },
    CoreInfo {
        id: "Game-Bub.GBA",
        name: "Game Boy Advance",
        author: "Game Bub",
        files: &[
            CoreFile {
                id: 0,
                label: "ROM",
                extensions: &[".gba"],
                asset_path: None,

                optional: true,
                read_only: true,
                user_selected: true,
                dependent_on_0: false,
                initialize: false,

                address: 0x3000_0000, // SDRAM
                max_size: 32 * 1024 * 1024,
                exact_size: 0,
                max_transfer_speed: 20_000, // 20 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits32,
            },
            CoreFile {
                id: 1,
                label: "Save",
                extensions: &[".sav"],
                asset_path: None,

                optional: true,
                read_only: false,
                user_selected: false,
                dependent_on_0: true,
                initialize: true,

                address: 0x4000_0000, // SRAM
                max_size: 128 * 1024 + 16,
                exact_size: 0,
                max_transfer_speed: 10_000, // 10 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits16,
            },
            CoreFile {
                id: 2,
                label: "BIOS",
                extensions: &[".bin"],
                asset_path: None, // TODO

                optional: false,
                read_only: true,
                user_selected: false,
                dependent_on_0: false,
                initialize: false,

                address: 0x1000_0000,
                max_size: 0,
                exact_size: 16 * 1024,
                max_transfer_speed: 20_000, // 20 MB/s
                transfer_word_size: fpga::FpgaSpiWordSize::Bits32,
            },
        ],
    },
];

pub fn get_core_info(id: &str) -> Option<&'static CoreInfo> {
    CORES.iter().find(|x| x.id == id)
}

/// Get a list of all available cores.
pub fn list_cores() -> Vec<CoreListEntry> {
    // Start with built-in cores.
    let mut cores = vec![
        CoreListEntry {
            id: "Game-Bub.GB".try_into().unwrap(),
            name: "Game Boy / Game Boy Color".try_into().unwrap(),
            author: "Game Bub".try_into().unwrap(),
        },
        CoreListEntry {
            id: "Game-Bub.GBA".try_into().unwrap(),
            name: "Game Boy Advance".try_into().unwrap(),
            author: "Game Bub".try_into().unwrap(),
        },
    ];

    // Iterate over possible core directories.
    if let Ok(entries) = std::fs::read_dir(DIR_CORES) {
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    log::warn!("Failed to list core file");
                    continue;
                }
            };

            // Skip non-directories
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            let mut path = entry.path();
            path.push("core.json");

            let file = match File::open(&path) {
                Ok(file) => file,
                Err(_) => {
                    log::warn!("Failed to open core file");
                    continue;
                }
            };

            /// Helper struct to extract only high-level info from core
            #[derive(Deserialize)]
            struct MinimalCoreInfo {
                metadata: CoreListEntry,
            }

            let reader = BufReader::with_capacity(256, file);
            match serde_json::from_reader::<_, MinimalCoreInfo>(reader) {
                Ok(info) => cores.push(info.metadata),
                Err(e) => {
                    let filename = path.file_name().unwrap_or_default();
                    log::warn!("Error parsing core file {filename:?}: {e:?}");
                    continue;
                }
            };
        }
    }

    cores.sort_by(|a, b| a.name.cmp(&b.name));
    cores
}
