pub mod animation;
#[cfg(feature = "desktop")]
pub mod app;
pub mod config;
// lane: input-gestures
pub mod gestures;
// lane: persistence-ipc
pub mod ipc;
pub mod layout;
pub mod model;
pub mod platform;
pub mod pointer;
pub mod rules;
pub mod screenshot; // lane: rules-spawn-screenshot
pub mod shortcuts;
pub mod spawn; // lane: rules-spawn-screenshot

pub mod preview;

#[cfg(feature = "desktop")]
pub mod terminal;

pub mod ui_animation; // lane: ui-animation
