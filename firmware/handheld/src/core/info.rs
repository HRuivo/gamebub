use arrayvec::{ArrayString, ArrayVec};
use serde::Deserialize;
use std::{
    fs::File,
    io::{BufReader, ErrorKind},
    path::{Path, PathBuf},
};

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
    pub id: ArrayString<32>,
    pub name: ArrayString<32>,
    pub author: ArrayString<32>,
    pub files: ArrayVec<CoreFile, 8>,
    pub settings: Vec<CoreSetting>,
    pub bitstream: PathBuf,
}

#[derive(Clone, Debug)]
pub struct CoreSetting {
    pub id: u16,
    pub label: ArrayString<32>,
    pub address: u32,
    pub items: Vec<CoreSettingItem>,
    pub default: u32,
}

#[derive(Clone, Debug)]
pub struct CoreSettingItem {
    pub label: ArrayString<32>,
    pub value: u32,
}

#[derive(Deserialize)]
struct JsonCoreSettings {
    settings: Vec<JsonCoreSetting>,
}

#[derive(Deserialize)]
struct JsonCoreSetting {
    id: u16,
    label: ArrayString<32>,
    address: String,
    #[serde(rename = "type")]
    kind: String,
    items: Vec<JsonCoreSettingItem>,
    default: String,
}

#[derive(Deserialize)]
struct JsonCoreSettingItem {
    label: ArrayString<32>,
    value: String,
}

#[derive(Deserialize)]
struct JsonCoreFiles {
    files: Vec<JsonCoreFile>,
}

#[derive(Deserialize)]
struct JsonCoreFile {
    id: u16,
    label: ArrayString<16>,
    #[serde(default)]
    filename: Option<ArrayString<32>>,
    #[serde(default)]
    extensions: Vec<ArrayString<8>>,
    #[serde(default = "default_true")]
    optional: bool,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    user_selected: bool,
    #[serde(default)]
    dependent_on_0: bool,
    #[serde(default)]
    initialize: bool,
    address: JsonU32,
    #[serde(default)]
    max_size: JsonU32,
    #[serde(default)]
    exact_size: JsonU32,
    #[serde(default = "default_max_transfer_speed")]
    max_transfer_speed: u32,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonU32 {
    Number(u32),
    String(String),
}

impl Default for JsonU32 {
    fn default() -> Self {
        Self::Number(0)
    }
}

impl JsonU32 {
    fn parse(self) -> Result<u32, String> {
        match self {
            Self::Number(value) => Ok(value),
            Self::String(value) => parse_u32(&value),
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_max_transfer_speed() -> u32 {
    5_000
}

#[allow(unused)]
pub struct CoreFile {
    pub id: u16,
    pub label: ArrayString<16>,
    /// If set, the file will be loaded from this path relative to the asset path.
    pub filename: Option<ArrayString<32>>,

    /// List of file extensions (optional).
    pub extensions: ArrayVec<ArrayString<8>, 4>,

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

    /// The address to load the file to.
    /// TODO: support hex-string
    pub address: u32,
    /// Maximum size of the file.
    /// TODO: support hex-string
    pub max_size: u32,
    /// Exact size of the file.
    /// TODO: support hex-string
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

fn load_files_metadata(core_dir: &Path) -> Result<ArrayVec<CoreFile, 8>, String> {
    let file = match File::open(core_dir.join("files.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(ArrayVec::new()),
        Err(e) => return Err(format!("Failed to open files.json: {e}")),
    };
    let reader = BufReader::with_capacity(256, file);
    let json: JsonCoreFiles =
        serde_json::from_reader(reader).map_err(|e| format!("Failed to parse files.json: {e}"))?;

    if json.files.len() > 8 {
        return Err(format!(
            "files.json contains {} files; at most 8 are supported",
            json.files.len()
        ));
    }

    let mut files = ArrayVec::new();
    for file in json.files {
        if files
            .iter()
            .any(|existing: &CoreFile| existing.id == file.id)
        {
            return Err(format!("Duplicate file ID {}", file.id));
        }
        if file.extensions.len() > 4 {
            return Err(format!(
                "File {} contains {} extensions; at most 4 are supported",
                file.id,
                file.extensions.len()
            ));
        }
        if file.user_selected && file.extensions.is_empty() {
            return Err(format!(
                "User-selected file {} must define at least one extension",
                file.id
            ));
        }
        if file.dependent_on_0 && file.extensions.len() != 1 {
            return Err(format!(
                "File {} depends on file 0 and must define exactly one extension",
                file.id
            ));
        }
        if file.id == 0 && file.dependent_on_0 {
            return Err("File 0 cannot depend on itself".to_string());
        }
        if file.max_transfer_speed == 0 {
            return Err(format!(
                "File {} max_transfer_speed must be greater than zero",
                file.id
            ));
        }

        let address = file
            .address
            .parse()
            .map_err(|e| format!("Invalid address for file {}: {e}", file.id))?;
        let max_size = file
            .max_size
            .parse()
            .map_err(|e| format!("Invalid max_size for file {}: {e}", file.id))?;
        let exact_size = file
            .exact_size
            .parse()
            .map_err(|e| format!("Invalid exact_size for file {}: {e}", file.id))?;
        if max_size != 0 && exact_size > max_size {
            return Err(format!(
                "File {} exact_size ({exact_size}) exceeds max_size ({max_size})",
                file.id
            ));
        }
        if address >= 0xF000_0000 {
            return Err(format!(
                "Address for file {} is in the reserved FPGA framework range: {address:#010x}",
                file.id
            ));
        }
        let transfer_size = exact_size.max(max_size);
        if transfer_size != 0 && address.checked_add(transfer_size - 1).is_none() {
            return Err(format!("Address range for file {} overflows", file.id));
        }

        files.push(CoreFile {
            id: file.id,
            label: file.label,
            filename: file.filename,
            extensions: file.extensions.into_iter().collect(),
            optional: file.optional,
            read_only: file.read_only,
            user_selected: file.user_selected,
            dependent_on_0: file.dependent_on_0,
            initialize: file.initialize,
            address,
            max_size,
            exact_size,
            max_transfer_speed: file.max_transfer_speed,
            // The generic host-memory path currently uses 32-bit transfers.
            transfer_word_size: fpga::FpgaSpiWordSize::Bits32,
        });
    }

    if files.iter().any(|file| file.dependent_on_0) && !files.iter().any(|file| file.id == 0) {
        return Err("A file depends on file 0, but file 0 is not defined".to_string());
    }

    Ok(files)
}

fn load_settings_metadata(core_dir: &Path) -> Result<Vec<CoreSetting>, String> {
    let file = match File::open(core_dir.join("settings.json")) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Failed to open settings.json: {e}")),
    };
    let reader = BufReader::with_capacity(256, file);
    let json: JsonCoreSettings = serde_json::from_reader(reader)
        .map_err(|e| format!("Failed to parse settings.json: {e}"))?;

    json.settings
        .into_iter()
        .map(|setting| {
            if setting.kind != "list" {
                return Err(format!(
                    "Unsupported setting type '{}' for setting {}",
                    setting.kind, setting.id
                ));
            }
            let items = setting
                .items
                .into_iter()
                .map(|item| {
                    Ok(CoreSettingItem {
                        label: item.label,
                        value: parse_u32(&item.value)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            if items.is_empty() {
                return Err(format!("Setting {} has no list items", setting.id));
            }
            let default = parse_u32(&setting.default)?;
            if !items.iter().any(|item| item.value == default) {
                return Err(format!(
                    "Default value for setting {} is not in its item list",
                    setting.id
                ));
            }
            let address = parse_u32(&setting.address)?;
            if address & 0x3 != 0 {
                return Err(format!(
                    "Address for setting {} must be 32-bit aligned, got {address:#010x}",
                    setting.id
                ));
            }
            // The handheld FPGA framework reserves the entire 0xFxxx_xxxx
            // range. Core-provided metadata must never be able to write to
            // framework control, overlay, or framebuffer registers.
            if address >= 0xF000_0000 {
                return Err(format!(
                    "Address for setting {} is in the reserved FPGA framework range: {address:#010x}",
                    setting.id
                ));
            }
            Ok(CoreSetting {
                id: setting.id,
                label: setting.label,
                address,
                items,
                default,
            })
        })
        .collect()
}

fn parse_u32(value: &str) -> Result<u32, String> {
    let (digits, radix) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or((value, 10), |digits| (digits, 16));
    u32::from_str_radix(digits, radix)
        .map_err(|_| format!("Invalid 32-bit setting value '{value}'"))
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

/// Get full information for a core.
/// TODO: better error type?
pub fn get_core(id: &str) -> Result<CoreInfo, String> {
    // Handle built-in cores.
    match id {
        "Game-Bub.GB" => return Ok(crate::bitstream::gameboy::Gameboy::get_core_info()),
        "Game-Bub.GBA" => return Ok(crate::bitstream::gba::Gba::get_core_info()),
        _ => {}
    }

    #[derive(Deserialize)]
    struct JsonCoreMetadata {
        pub id: ArrayString<32>,
        pub name: ArrayString<32>,
        pub author: ArrayString<32>,
    }

    #[derive(Deserialize)]
    struct JsonCoreBitstream {
        pub target: ArrayString<16>,
        pub filename: ArrayString<32>,
    }

    #[derive(Deserialize)]
    struct JsonCoreInfo {
        metadata: JsonCoreMetadata,
        bitstreams: Vec<JsonCoreBitstream>,
    }

    let mut core_dir = PathBuf::from(DIR_CORES);
    core_dir.push(id);

    // Read core.json
    let file = File::open(core_dir.join("core.json")).map_err(|_| "Failed to open core.json")?;
    let reader = BufReader::with_capacity(256, file);
    let json_core: JsonCoreInfo =
        serde_json::from_reader(reader).map_err(|e| format!("Failed to parse core.json: {e}"))?;

    let settings = load_settings_metadata(&core_dir)?;

    // Find the right bitstream (TODO: use a visitor that extracts the right one).
    let bitstream = json_core
        .bitstreams
        .iter()
        .find_map(|b| {
            if b.target.as_str() == get_device_target() && b.filename.ends_with(".bit") {
                Some(b.filename)
            } else {
                None
            }
        })
        .ok_or("No compatible bitstream")?;

    let files = load_files_metadata(&core_dir)?;

    Ok(CoreInfo {
        id: json_core.metadata.id,
        name: json_core.metadata.name,
        author: json_core.metadata.author,
        files,
        settings,
        bitstream: core_dir.join(bitstream),
    })
}

pub fn list_configurable_cores() -> Vec<CoreInfo> {
    list_cores()
        .into_iter()
        .filter_map(|entry| match get_core(entry.id.as_str()) {
            Ok(core) if !core.settings.is_empty() => Some(core),
            Ok(_) => None,
            Err(e) => {
                log::warn!("Failed to load settings for core '{}': {e}", entry.id);
                None
            }
        })
        .collect()
}

fn get_device_target() -> &'static str {
    #[cfg(feature = "rev1")]
    const TARGET: &'static str = "gamebub_rev1";
    #[cfg(feature = "rev2")]
    const TARGET: &'static str = "gamebub_rev2";
    #[cfg(feature = "rev3")]
    const TARGET: &'static str = "gamebub_rev3";
    #[cfg(feature = "rev4")]
    const TARGET: &'static str = "gamebub_rev4";

    TARGET
}
