//! Workspace detail state; independent of overlays and keyboard routing.
use super::{UiState, reader::ReaderContent};
use bone_app::SessionSeq;

#[derive(Debug)]
pub(crate) struct ReaderState {
    pub(crate) content: ReaderContent,
    pub(crate) scroll: usize,
}

pub(super) fn open_history(state: &mut UiState, sequence: SessionSeq) {
    let Some(content) = state.selected_ui().and_then(|ui| {
        ui.transcript
            .find(sequence)
            .and_then(|entry| ReaderContent::from_history(ui.id, entry))
    }) else {
        return;
    };
    pin_reading(state);
    state.details = Some(ReaderState { content, scroll: 0 });
}

pub(super) fn open_job(state: &mut UiState, job: bone_app::JobRef) {
    let Some(content) = state
        .selected_ui()
        .and_then(|ui| ui.snapshot.as_ref())
        .and_then(|snapshot| ReaderContent::from_job(snapshot, job))
    else {
        return;
    };
    pin_reading(state);
    state.details = Some(ReaderState { content, scroll: 0 });
}

pub(super) fn refresh_reader(state: &mut UiState, snapshot: &bone_app::SessionView) {
    if let Some(reader) = &mut state.details {
        reader.content.refresh_job(snapshot);
    }
}

pub(super) fn scroll_reader(state: &mut UiState, amount: isize, max: usize) {
    if let Some(reader) = &mut state.details {
        reader.scroll = reader
            .scroll
            .min(max)
            .saturating_add_signed(amount)
            .min(max);
    }
}

pub(super) fn pin_reading(state: &mut UiState) {
    if let Some(ui) = state.selected_ui_mut() {
        ui.transcript.pin_reading();
    }
}
