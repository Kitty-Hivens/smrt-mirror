//! Data model layer: wire DTOs, the admin-authored pack config, and pure
//! version rules. No I/O. Submodules are flattened here so callers reach
//! every model type via `crate::domain::*`.

pub mod diff;
/// Not flattened: `settle` and `untagged` are only meaningful beside the map
/// they are settling, so they are read at `i18n::` rather than loose among the
/// model types.
pub mod i18n;
pub mod manifest;
pub mod pack;
pub mod server;
pub mod side;
pub mod version;

pub use diff::*;
pub use manifest::*;
pub use pack::*;
pub use server::*;
pub use side::*;
pub use version::*;
