mod app_icon;
mod epi_integration;
mod event_loop_context;
pub mod run;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

/// File storage which can be used by native backends.
///
/// Also compiled in tests so the persistence format can be tested without
/// enabling the `persistence` feature.
#[cfg(any(feature = "persistence", test))]
pub mod file_storage;

pub(crate) mod winit_integration;

#[cfg(feature = "glow")]
mod glow_integration;

#[cfg(feature = "wgpu_no_default_features")]
mod wgpu_integration;
