use std::sync::Arc;

use axum::extract::FromRef;

use crate::{Config, Context};

pub type AppContext = Arc<Context>;
pub type AppConfig = Config;

impl FromRef<AppContext> for AppConfig {
    fn from_ref(app_context: &AppContext) -> AppConfig {
        app_context.config().clone()
    }
}
