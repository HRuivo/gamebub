use std::{cell::RefCell, rc::Rc};

use super::super::slint::Backend;
use slint::{ComponentHandle, SharedString};

use crate::{device::Device, worker};

use super::UiState;

impl UiState {
    /// Set up the "Cores" screen.
    pub(super) fn setup_cores(&mut self, state: &Rc<RefCell<UiState>>, _device: &mut Device) {
        let root = self.root.unwrap();
        let backend = root.global::<Backend>();

        let state_ = state.clone();
        backend.on_core_run(move |core_id| {
            let mut state = state_.borrow_mut();
            state.cores_handle_run(core_id);
        });
    }

    pub fn cores_handle_run(&mut self, core_id: SharedString) {
        worker::send(worker::Message::RunCore(core_id.to_string()));
    }
}
