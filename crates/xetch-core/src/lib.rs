//! Shared disk-image application core. No GUI dependencies.
mod app;
mod error;
mod ext4_worker;
mod ext_acl;
mod extended;
mod files;
mod nbd;
mod ntfs;
mod partition;
mod staging;
mod tools;
mod validation;
mod workspace;

pub use app::execute;
pub use error::{Error, ErrorBody, Result};
pub mod child;
