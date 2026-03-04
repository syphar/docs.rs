pub(crate) mod templates;
pub(crate) mod web_page;

pub(crate) use templates::TemplateData;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GlobalAlert {
    pub(crate) url: String,
    pub(crate) text: String,
    pub(crate) css_class: String,
}
