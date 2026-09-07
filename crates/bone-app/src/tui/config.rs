/// Compatibility name for the terminal presentation settings. The owning
/// type now lives in `bone-app`, where SettingsService can apply it in the
/// active TUI instead of treating it as a startup-only configuration value.
pub use crate::TuiDisplaySettings as TuiConfig;
