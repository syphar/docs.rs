mod canonical_url;
mod etag;
mod forwarded_host;
mod forwarded_proto;
mod if_none_match;
mod surrogate_key;
#[cfg(test)]
mod testing;

pub use canonical_url::CanonicalUrl;
pub use etag::{ETagComputer, compute_etag};
pub use forwarded_host::{X_FORWARDED_HOST, XForwardedHost};
pub use forwarded_proto::{X_FORWARDED_PROTO, XForwardedProto};
pub use headers::{ETag, Host};
pub use if_none_match::IfNoneMatch;
pub use surrogate_key::{SURROGATE_KEY, SurrogateKey, SurrogateKeys};

use http::HeaderName;

/// Fastly's Surrogate-Control header
/// https://www.fastly.com/documentation/reference/http/http-headers/Surrogate-Control/
pub static SURROGATE_CONTROL: HeaderName = HeaderName::from_static("surrogate-control");

/// X-Robots-Tag header for search engines.
pub static X_ROBOTS_TAG: HeaderName = HeaderName::from_static("x-robots-tag");
