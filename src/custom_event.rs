//! Custom event type for Ruffle on Android

use ruffle_core::events::{KeyDescriptor, TextControlCode};
use ruffle_render::quality::StageQuality;

use crate::PlayerRunnable;

/// User-defined events.
pub enum RuffleEvent {
    /// Indicates that a task is ready to be polled.
    TaskPoll(PlayerRunnable),
    VirtualKeyEvent {
        down: bool,
        key_descriptor: KeyDescriptor,
    },
    TextInput(String),
    TextControl {
        code: TextControlCode,
        repeat_count: u32,
    },
    SetStageQuality(StageQuality),
    RunContextMenuCallback(usize),
    ClearContextMenu,
    RequestContextMenu,
    ReloadMovie,
}
