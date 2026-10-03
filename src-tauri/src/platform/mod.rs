#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{Backend, hook, splitter};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Backend;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::Backend;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
compile_error!("e-desktop currently supports desktop Windows, Linux X11, and macOS targets.");

pub(crate) fn own_terminal_title(title: &str) -> bool {
    #[cfg(feature = "desktop")]
    { crate::terminal::eligible_title(title) }
    #[cfg(not(feature = "desktop"))]
    { let _ = title; false }
}
