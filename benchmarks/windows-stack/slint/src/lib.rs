//! Platform-neutral QuickNote core and Slint UI shared by desktop and future mobile shells.

pub mod core;
pub mod store;

// The generated component contains no Windows API and is reusable by mobile entrypoints.
slint::include_modules!();
