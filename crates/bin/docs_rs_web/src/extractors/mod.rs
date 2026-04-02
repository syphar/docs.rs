mod context;
mod host;
mod path;
pub(crate) mod rustdoc;
pub(crate) mod rustdoc_redirector;

pub(crate) use context::DbConnection;
pub(crate) use host::RequestedHost;
pub(crate) use path::{Path, WantedCompression};
