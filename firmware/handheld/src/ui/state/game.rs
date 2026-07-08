use std::{cell::RefCell, rc::Rc, time::Duration};

use super::super::slint::Backend;
use slint::{ComponentHandle, Timer};

use crate::{core::CoreManager, device::Device, ui::slint::ScreenId, worker};

use super::UiState;

impl UiState {
    /// Set up the "Game" screen.
    pub(super) fn setup_game(&mut self, state: &Rc<RefCell<UiState>>, _device: &mut Device) {
        let root = self.root.unwrap();
        let backend = root.global::<Backend>();

        let state_ = state.clone();
        backend.on_game_set_paused(move |paused| {
            let needs_persist = {
                let mut manager = CoreManager::lock();
                let bitstream = manager.current_bitstream().unwrap();
                bitstream.set_paused(paused).unwrap();
                bitstream.needs_save_persist()
            };

            if paused && needs_persist {
                let state = state_.borrow_mut();
                let root = state.root.unwrap();
                let backend = root.global::<Backend>();
                backend.set_status_is_saving(true);
                worker::send(worker::Message::SaveGame);
            }
        });

        backend.on_game_reset(move || {
            CoreManager::lock()
                .current_bitstream()
                .unwrap()
                .reset()
                .unwrap();
        });

        let state_ = state.clone();
        backend.on_game_exit(move || {
            worker::send(worker::Message::ExitCore);
            // Give it a moment to start loading the boot bitstream (avoid screen flash)
            std::thread::sleep(Duration::from_millis(100));
            // Go back to the main menu
            let root = {
                let state = state_.borrow_mut();
                state.root.unwrap()
            };
            root.invoke_set_screen(ScreenId::MainMenu);
        });
    }

    pub fn game_on_saved(&mut self) {
        // Ensure that the save icon is shown for a visible amount of time.
        let window = self.root.upgrade().unwrap();
        Timer::single_shot(Duration::from_millis(1000), move || {
            window.global::<Backend>().set_status_is_saving(false);
        });
    }
}
