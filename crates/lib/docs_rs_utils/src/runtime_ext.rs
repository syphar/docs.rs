#![allow(clippy::disallowed_types)]

use tokio::{
    runtime::{self, TryCurrentError},
    task::JoinHandle,
};
use tracing::Instrument as _;

/// Newtype around `tokio::runtime::Handle` that adds
/// missing integration with tracing spans.
#[derive(Debug, Clone)]
pub struct Handle(runtime::Handle);

impl Handle {
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        runtime::Handle::block_on(self.as_handle(), future.in_current_span())
    }

    pub fn as_handle(&self) -> &runtime::Handle {
        &self.0
    }

    #[track_caller]
    pub fn current() -> Self {
        runtime::Handle::current().into()
    }

    pub fn try_current() -> Result<Self, TryCurrentError> {
        runtime::Handle::try_current().map(Into::into)
    }

    #[track_caller]
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.spawn(future.in_current_span())
    }
}

impl From<runtime::Handle> for Handle {
    fn from(handle: runtime::Handle) -> Self {
        Handle(handle)
    }
}
