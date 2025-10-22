use crate::web::encode_url_path;
use http::{Uri, uri::PathAndQuery};
use std::{borrow::Borrow, fmt::Display, iter, ops::Deref};
use url::form_urlencoded;

/// internal wrapper around `http::Uri` with some convenience functions.
///
/// Ensures that the path part is always properly percent-encoded, including some characters
/// that http::Uri would allow, but we still want to encode, like umlauts.
#[derive(Debug, Clone)]
pub struct EscapedURI {
    uri: Uri,
    fragment: Option<String>,
}

impl EscapedURI {
    pub fn from_uri(uri: Uri) -> Self {
        if uri.path_and_query().is_some() {
            if uri.path() == encode_url_path(uri.path()) {
                Self {
                    uri,
                    fragment: None,
                }
            } else {
                let mut parts = uri.into_parts();

                parts.path_and_query = Some(
                    PathAndQuery::from_maybe_shared(
                        parts
                            .path_and_query
                            .take()
                            .map(|pq| {
                                format!(
                                    "{}{}",
                                    encode_url_path(pq.path()),
                                    pq.query().map(|q| format!("?{}", q)).unwrap_or_default(),
                                )
                            })
                            .unwrap_or_default(),
                    )
                    .expect("can't fail since we encode the path ourselves"),
                );

                Self {
                    uri: Uri::from_parts(parts)
                        .expect("everything is coming from a previous Uri, or encoded here"),
                    fragment: None,
                }
            }
        } else {
            Self {
                uri,
                fragment: None,
            }
        }
    }

    pub fn from_path(path: impl AsRef<str>) -> Self {
        Self {
            uri: Uri::builder()
                .path_and_query(encode_url_path(path.as_ref()))
                .build()
                .expect("this can never fail because we encode the path"),
            fragment: None,
        }
    }

    pub fn from_path_and_raw_query(
        path: impl AsRef<str>,
        raw_query: Option<impl AsRef<str>>,
    ) -> Self {
        Self::from_path(path).append_raw_query(raw_query)
    }

    pub(crate) fn from_path_and_query<P, I, K, V>(path: P, queries: I) -> Self
    where
        P: AsRef<str>,
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        Self::from_path(path).append_query_pairs(queries)
    }

    pub fn path(&self) -> &str {
        self.uri.path()
    }

    /// extend the query part of the Uri with the given raw query string.
    ///
    /// Will parse & re-encode the string, which is why the method is infallible (I think)
    pub fn append_raw_query(self, raw_query: Option<impl AsRef<str>>) -> Self {
        let raw_query = match raw_query {
            Some(ref q) => q.as_ref(),
            None => return self,
        };

        self.append_query_pairs(form_urlencoded::parse(raw_query.as_bytes()))
    }

    pub fn append_query_pairs<I, K, V>(self, new_query_args: I) -> Self
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut new_query_args = new_query_args.into_iter().peekable();
        if new_query_args.peek().is_none() {
            return self;
        }

        let mut serializer = form_urlencoded::Serializer::new(String::new());

        if let Some(existing_query_args) = self.uri.query() {
            serializer.extend_pairs(form_urlencoded::parse(existing_query_args.as_bytes()));
        }

        serializer.extend_pairs(new_query_args);

        let mut parts = self.uri.into_parts();

        parts.path_and_query = Some(
            PathAndQuery::from_maybe_shared(format!(
                "{}?{}",
                parts
                    .path_and_query
                    .map(|pg| pg.path().to_owned())
                    .unwrap_or_default(),
                serializer.finish(),
            ))
            .expect("can't fail since all the data is either coming from a previous Uri, or we encode it ourselves")
        );

        Self::from_uri(
            Uri::from_parts(parts).expect(
                "can't fail since data is either coming from an Uri, or encoded by ourselves.",
            ),
        )
    }

    /// extend query part
    pub fn append_query_pair(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.append_query_pairs(iter::once((key, value)))
    }

    pub fn into_inner(self) -> Uri {
        self.uri
    }

    pub(crate) fn with_fragment(mut self, fragment: impl Into<String>) -> Self {
        self.fragment = Some(fragment.into());
        self
    }
}

impl Display for EscapedURI {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(fragment) = &self.fragment {
            write!(f, "{}#{}", self.uri, fragment)
        } else {
            write!(f, "{}", self.uri)
        }
    }
}

impl TryFrom<EscapedURI> for Uri {
    type Error = anyhow::Error;

    fn try_from(value: EscapedURI) -> Result<Self, Self::Error> {
        if let Some(fragment) = value.fragment {
            Err(anyhow::anyhow!(
                "can't convert EscapedURI with fragment '{}' into Uri",
                fragment
            ))
        } else {
            Ok(value.uri)
        }
    }
}

impl Deref for EscapedURI {
    type Target = Uri;

    fn deref(&self) -> &Self::Target {
        &self.uri
    }
}

impl From<Uri> for EscapedURI {
    fn from(value: Uri) -> Self {
        Self::from_uri(value)
    }
}

impl PartialEq for EscapedURI {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri && self.fragment == other.fragment
    }
}

impl PartialEq<Uri> for EscapedURI {
    fn eq(&self, other: &Uri) -> bool {
        &self.uri == other && self.fragment.is_none()
    }
}

impl PartialEq<&str> for EscapedURI {
    fn eq(&self, other: &&str) -> bool {
        &self.uri.to_string() == *other && self.fragment.is_none()
    }
}

impl PartialEq<String> for EscapedURI {
    fn eq(&self, other: &String) -> bool {
        &self.uri.to_string() == other && self.fragment.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::EscapedURI;
    use crate::web::{cache::CachePolicy, error::AxumNope};
    use axum::response::IntoResponse as _;
    use http::Uri;
    use test_case::test_case;

    #[test]
    fn test_redirect_error_encodes_url_path() {
        let response = AxumNope::Redirect(
            EscapedURI::from_path("/something>"),
            CachePolicy::ForeverInCdnAndBrowser,
        )
        .into_response();

        assert_eq!(response.status(), 302);
        assert_eq!(response.headers().get("Location").unwrap(), "/something%3E");
    }

    #[test_case("/something" => "/something")]
    #[test_case("/something>" => "/something%3E")]
    fn test_escaped_uri_encodes_from_path(input: &str) -> String {
        let escaped = EscapedURI::from_path(input);
        escaped.path().to_owned()
    }

    #[test_case("/something" => "/something"; "plain path")]
    #[test_case("/somethingäöü" => "/something%C3%A4%C3%B6%C3%BC"; "path with umlauts")]
    fn test_escaped_uri_encodes_path_from_uri(path: &str) -> String {
        let uri: Uri = path.parse().unwrap();
        let escaped = EscapedURI::from_uri(uri);
        escaped.path().to_string()
    }

    #[test]
    fn test_escaped_uri_from_uri_with_query_args() {
        let uri: Uri = "/something?key=value&foo=bar".parse().unwrap();
        let escaped = EscapedURI::from_uri(uri);
        assert_eq!(escaped.path(), "/something");
        assert_eq!(escaped.query(), Some("key=value&foo=bar"));
    }

    #[test_case("/something>")]
    #[test_case("/something?key=<value&foo=\rbar")]
    fn test_escaped_uri_encodes_path_from_uri_invalid(input: &str) {
        // things that are invalid URIs should error out,
        // so are unusable for EscapedURI::from_uri`
        //
        // More to test if my assumption is correct that we don't have to re-encode.
        assert!(input.parse::<Uri>().is_err());
    }

    #[test_case(
        "/something", "key=value&foo=bar"
        => ("/something".into(), "key=value&foo=bar".into());
        "plain convert"
    )]
    #[test_case(
        "/something", "value=foo\rbar&key=<value"
        => ("/something".into(), "value=foo%0Dbar&key=%3Cvalue".into());
        "invalid query gets re-encoded without error"
    )]
    fn test_escaped_uri_from_raw_query(path: &str, query: &str) -> (String, String) {
        let uri = EscapedURI::from_path_and_raw_query(path, Some(query));

        (uri.path().to_owned(), uri.query().unwrap().to_owned())
    }

    #[test]
    fn test_escaped_uri_from_query() {
        let uri =
            EscapedURI::from_path_and_query("/something", &[("key", "value"), ("foo", "bar")]);

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value&foo=bar"));
    }

    #[test]
    fn test_escaped_uri_from_query_with_chars_to_encode() {
        let uri =
            EscapedURI::from_path_and_query("/something", &[("key", "value>"), ("foo", "\rbar")]);

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value%3E&foo=%0Dbar"));
    }

    #[test]
    fn test_escaped_uri_append_query_pairs_without_path() {
        let uri = Uri::builder().build().unwrap();

        let parts = uri.into_parts();
        // `append_query_pairs` has a special case when path_and_query is `None`,
        // which I want to test here.
        assert!(parts.path_and_query.is_none());

        // also tests appending query pairs if there are no existing query args
        let uri = EscapedURI::from_uri(Uri::from_parts(parts).unwrap())
            .append_query_pairs(&[("foo", "bar"), ("bar", "baz")]);

        assert_eq!(uri.path(), "/");
        assert_eq!(uri.query(), Some("foo=bar&bar=baz"));
    }

    #[test]
    fn test_escaped_uri_append_query_pairs() {
        let uri = EscapedURI::from_path_and_query("/something", &[("key", "value")])
            .append_query_pairs(&[("foo", "bar"), ("bar", "baz")])
            .append_query_pair("last", "one");

        assert_eq!(uri.path(), "/something");
        assert_eq!(uri.query(), Some("key=value&foo=bar&bar=baz&last=one"));
    }
}
