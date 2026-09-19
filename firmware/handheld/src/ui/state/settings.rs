use std::cell::RefCell;
use std::rc::Rc;

use super::super::slint::Backend;
use crate::device::Device;
use crate::ui::slint::ScreenId;
use crate::ui::state::{SettingDatetime, SettingEntry, SettingType, SettingValue};
use settings::Page;
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, ToSharedString};
use time::OffsetDateTime;

use super::UiState;

mod settings {
    use std::{cell::Cell, rc::Rc};

    use crate::core;
    use crate::kvs::{keys, KvsKey};
    use crate::ui::state::ScreenId;

    pub struct Page {
        pub name: String,
        pub entries: Vec<Entry>,
    }

    pub enum Entry {
        Checkbox {
            name: String,
            key: &'static KvsKey<bool>,
        },
        List {
            name: String,
            key: &'static KvsKey<i32>,
            choices: Vec<String>,
        },
        CoreList {
            name: String,
            core_id: String,
            setting_id: u16,
            choices: Vec<String>,
            selected: Cell<i32>,
        },
        SystemDatetime {
            name: String,
        },
        Subpage {
            name: String,
            page: Rc<Page>,
        },
        Screen {
            name: String,
            screen: ScreenId,
        },
    }

    fn general_page() -> Rc<Page> {
        Rc::new(Page {
            name: "General".to_string(),
            entries: vec![
                Entry::SystemDatetime {
                    name: "Date and Time (UTC)".to_string(),
                },
                Entry::List {
                    name: "Startup Action".to_string(),
                    key: &keys::STARTUP_ACTION,
                    choices: vec!["Main Menu".to_string(), "Run Cartridge".to_string()],
                },
            ],
        })
    }

    fn core_gb_page() -> Rc<Page> {
        Rc::new(Page {
            name: "Core: GB / GBC".to_string(),
            entries: vec![
                Entry::Checkbox {
                    name: "Enable GB mode".to_string(),
                    key: &keys::GB_IS_DMG,
                },
                Entry::List {
                    name: "GBC Color Corrections".to_string(),
                    key: &keys::CGB_COLOR_PROFILE,
                    choices: ["None", "GBC", "GBA", "GBA SP"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
                Entry::List {
                    name: "GB Color Palette".to_string(),
                    key: &keys::DMG_COLOR_PALETTE,
                    choices: ["Grayscale", "DMG Green", "GB Pocket"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
            ],
        })
    }

    fn core_gba_page() -> Rc<Page> {
        Rc::new(Page {
            name: "Core: GBA".to_string(),
            entries: vec![
                Entry::List {
                    name: "Color Corrections".to_string(),
                    key: &keys::GBA_COLOR_PROFILE,
                    choices: ["None", "GBA", "GBA SP", "NDS", "NDS Lite", "NSO GBA"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                },
                Entry::Checkbox {
                    name: "Enable Game Boy Player".to_string(),
                    key: &keys::GBA_ENABLE_GBP,
                },
                Entry::Checkbox {
                    name: "Warn about missing BIOS".to_string(),
                    key: &keys::GBA_BIOS_WARNING,
                },
            ],
        })
    }

    pub fn empty_page() -> Rc<Page> {
        Rc::new(Page {
            name: String::new(),
            entries: Vec::new(),
        })
    }

    pub fn root_page() -> Rc<Page> {
        let general = general_page();
        let core_gb = core_gb_page();
        let core_gba = core_gba_page();
        let mut entries = vec![
            Entry::Screen {
                name: "About".to_string(),
                screen: ScreenId::About,
            },
            Entry::Subpage {
                name: general.name.clone(),
                page: general,
            },
            Entry::Subpage {
                name: core_gb.name.clone(),
                page: core_gb,
            },
            Entry::Subpage {
                name: core_gba.name.clone(),
                page: core_gba,
            },
        ];

        for core in core::configurable_cores() {
            let page_name = format!("Core: {}", core.name);
            let page = Rc::new(Page {
                name: page_name.clone(),
                entries: core
                    .settings
                    .into_iter()
                    .map(|setting| Entry::CoreList {
                        name: setting.label,
                        core_id: core.id.clone(),
                        setting_id: setting.id,
                        choices: setting.choices,
                        selected: Cell::new(setting.selected as i32),
                    })
                    .collect(),
            });
            entries.push(Entry::Subpage {
                name: page_name,
                page,
            });
        }

        entries.push(Entry::Screen {
            name: "Diagnostics".to_string(),
            screen: ScreenId::Diagnostics,
        });
        Rc::new(Page {
            name: String::new(),
            entries,
        })
    }
}

pub struct SettingsState {
    model: Option<Rc<SettingsModel>>,
    page: Rc<Page>,
    stack: Vec<(Rc<Page>, usize)>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            model: None,
            page: settings::empty_page(),
            stack: Vec::new(),
        }
    }
}

impl UiState {
    /// Set up the "Settings" screen.
    pub(super) fn setup_settings(&mut self, state: &Rc<RefCell<UiState>>, _device: &mut Device) {
        let root = self.root.unwrap();
        let backend = root.global::<Backend>();

        let state_ = state.clone();
        backend.on_settings_changed(move |i, value| {
            let mut state = state_.borrow_mut();
            let Some(model) = state.settings.model.as_ref() else {
                return;
            };

            let action = model.changed(i as usize, value);
            match action {
                SettingsAction::None => {}
                SettingsAction::Subpage(page) => {
                    let entry = (state.settings.page.clone(), i as usize);
                    state.settings.stack.push(entry);
                    state.set_settings_page(page, 0);
                }
                SettingsAction::Screen(screen_id) => {
                    let root = state.root.unwrap();
                    std::mem::drop(state);
                    root.invoke_set_screen(screen_id);

                    // Workaround for issue where the title doesn't get updated
                    // (it's updated, but it doesn't take effect until the next render)
                    crate::ui::send(crate::ui::Message::Redraw);
                }
            }
        });

        let state_ = state.clone();
        backend.on_settings_back(move || {
            let mut state = state_.borrow_mut();
            match state.settings.stack.pop() {
                Some(previous) => {
                    state.set_settings_page(previous.0, previous.1);
                    true
                }
                None => false,
            }
        })
    }

    fn set_settings_page(&mut self, page: Rc<Page>, selected_item: usize) {
        let root = self.root.unwrap();
        let backend = root.global::<Backend>();
        let model = Rc::new(SettingsModel::new(page.clone()));
        self.settings.model = Some(model.clone());
        self.settings.page = page.clone();
        backend.set_settings(ModelRc::from(model));
        backend.set_settings_index(selected_item as i32);

        // Should be in Slint, but there isn't a good way to do it
        let mut title = "Settings".to_shared_string();
        if !page.name.is_empty() {
            title.push_str(": ");
            title.push_str(&page.name);
        }
        root.invoke_set_title(title);
    }

    pub(super) fn on_settings_enter(&mut self) {
        self.settings.stack.clear();
        self.set_settings_page(settings::root_page(), 0);
    }
}

pub struct SettingsModel {
    page: Rc<settings::Page>,
    notify: ModelNotify,
}

impl Model for SettingsModel {
    type Data = SettingEntry;

    fn row_count(&self) -> usize {
        self.page.entries.len()
    }

    fn row_data(&self, row: usize) -> Option<Self::Data> {
        let entry = self.page.entries.get(row)?;
        let row = match entry {
            settings::Entry::Checkbox { name, key } => SettingEntry {
                name: name.into(),
                r#type: SettingType::Checkbox,
                value: SettingValue {
                    bool_value: key.get().unwrap(),
                    ..SettingValue::default()
                },
                ..Default::default()
            },
            settings::Entry::List { name, key, choices } => SettingEntry {
                name: name.into(),
                r#type: SettingType::List,
                value: SettingValue {
                    int_value: key.get().unwrap(),
                    ..SettingValue::default()
                },
                choices: ModelRc::new(
                    choices
                        .iter()
                        .map(slint::SharedString::from)
                        .collect::<slint::VecModel<_>>(),
                ),
            },
            settings::Entry::CoreList {
                name,
                choices,
                selected,
                ..
            } => SettingEntry {
                name: name.into(),
                r#type: SettingType::List,
                value: SettingValue {
                    int_value: selected.get(),
                    ..SettingValue::default()
                },
                choices: ModelRc::new(
                    choices
                        .iter()
                        .map(slint::SharedString::from)
                        .collect::<slint::VecModel<_>>(),
                ),
            },
            settings::Entry::SystemDatetime { name } => SettingEntry {
                name: name.into(),
                r#type: SettingType::Datetime,
                value: SettingValue {
                    datetime_value: {
                        let dt = OffsetDateTime::now_utc();
                        SettingDatetime {
                            year: dt.year(),
                            month: dt.month() as i32,
                            day: dt.day() as i32,
                            hour: dt.hour() as i32,
                            min: dt.minute() as i32,
                            sec: dt.second() as i32,
                        }
                    },
                    ..SettingValue::default()
                },
                ..Default::default()
            },
            settings::Entry::Subpage { name, .. } => SettingEntry {
                name: name.into(),
                r#type: SettingType::Subpage,
                ..Default::default()
            },
            settings::Entry::Screen { name, .. } => SettingEntry {
                name: name.into(),
                r#type: SettingType::Subpage,
                ..Default::default()
            },
        };
        Some(row)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn core::any::Any {
        // a typical implementation just return `self`
        self
    }
}

impl SettingsModel {
    fn new(page: Rc<settings::Page>) -> Self {
        SettingsModel {
            page,
            notify: ModelNotify::default(),
        }
    }

    /// Notify that a setting has changed. Returns whether we navigated to a new page.
    fn changed(&self, index: usize, value: SettingValue) -> SettingsAction {
        let Some(entry) = self.page.entries.get(index) else {
            log::info!("Unknown setting changed: {} -> {:?}", index, value);
            return SettingsAction::None;
        };

        match entry {
            settings::Entry::Checkbox { key, .. } => key.set(&value.bool_value),
            settings::Entry::List { key, .. } => key.set(&value.int_value),
            settings::Entry::CoreList {
                core_id,
                setting_id,
                choices,
                selected,
                ..
            } => {
                let choice = value.int_value;
                if choice < 0 || choice as usize >= choices.len() {
                    log::warn!("Invalid setting choice: {choice}");
                } else if let Err(e) =
                    crate::core::set_configurable_setting(core_id, *setting_id, choice as usize)
                {
                    log::error!("Failed to save core setting: {e}");
                } else {
                    selected.set(choice);
                }
            }
            settings::Entry::SystemDatetime { .. } => {
                let dt = convert_settings_datetime(&value.datetime_value).unwrap();
                let dt = dt.replace_second(0).unwrap();
                let dt = dt.assume_utc();
                Device::lock().set_datetime(dt);
            }
            settings::Entry::Subpage { page, .. } => {
                return SettingsAction::Subpage(page.clone());
            }
            settings::Entry::Screen { screen, .. } => {
                return SettingsAction::Screen(*screen);
            }
        }
        self.notify.row_changed(index);
        SettingsAction::None
    }
}

enum SettingsAction {
    None,
    Subpage(Rc<Page>),
    Screen(ScreenId),
}

/// Helper function used by the UI to be able to correctly modify individual datetime components.
pub fn settings_datetime_add(source: SettingDatetime, delta: SettingDatetime) -> SettingDatetime {
    fn inner(
        source: &SettingDatetime,
        delta: SettingDatetime,
    ) -> time::Result<time::PrimitiveDateTime> {
        let mut dt = convert_settings_datetime(source)?;
        dt = dt.replace_day(1)?; // The day will be re-added later.
        dt = dt.replace_year(((dt.year() as i32) + delta.year).min(2100).max(2000))?;
        if delta.month < 0 {
            dt = dt.replace_month(dt.month().nth_prev((-delta.month) as u8))?;
        } else {
            dt = dt.replace_month(dt.month().nth_next(delta.month as u8))?;
        }
        dt = dt.replace_hour(((dt.hour() as i32) + delta.hour).rem_euclid(24) as u8)?;
        dt = dt.replace_minute(((dt.minute() as i32) + delta.min).rem_euclid(60) as u8)?;
        dt = dt.replace_second(((dt.second() as i32) + delta.sec).rem_euclid(60) as u8)?;
        let day_max = time::util::days_in_month(dt.month(), dt.year()) as i32;
        if delta.day == 0 {
            // If we aren't changing the day, clamp it to the maximum days in the month.
            dt = dt.replace_day(source.day.min(day_max) as u8)?;
        } else {
            dt = dt.replace_day((source.day + delta.day - 1).rem_euclid(day_max) as u8 + 1)?;
        }
        Ok(dt)
    }
    match inner(&source, delta) {
        Ok(dt) => SettingDatetime {
            year: dt.year(),
            month: dt.month() as i32,
            day: dt.day() as i32,
            hour: dt.hour() as i32,
            min: dt.minute() as i32,
            sec: dt.second() as i32,
        },
        Err(_) => {
            log::warn!("Invalid date");
            source
        }
    }
}

fn convert_settings_datetime(source: &SettingDatetime) -> time::Result<time::PrimitiveDateTime> {
    let date = time::Date::from_calendar_date(
        source.year,
        (source.month as u8).try_into()?,
        source.day as u8,
    )?;
    let time = time::Time::from_hms(source.hour as u8, source.min as u8, source.sec as u8)?;
    Ok(time::PrimitiveDateTime::new(date, time))
}
