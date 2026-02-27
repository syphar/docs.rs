use headers::{Error, Header};
use http::{HeaderName, HeaderValue, uri::Scheme};

pub static X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

/// Typed X-Forwarded-Proto header.
#[derive(Clone, Debug)]
pub struct XForwardedProto(Scheme);

impl XForwardedProto {
    pub fn proto(&self) -> &Scheme {
        &self.0
    }
}

impl Header for XForwardedProto {
    fn name() -> &'static HeaderName {
        &X_FORWARDED_PROTO
    }

    fn decode<'i, I: Iterator<Item = &'i HeaderValue>>(values: &mut I) -> Result<Self, Error> {
        let Some(value) = values.next() else {
            return Err(Error::invalid());
        };

        let value = value.as_bytes().trim_ascii();

        if value.is_empty() {
            return Err(Error::invalid());
        }

        let proto: Scheme = value.try_into().map_err(|_| Error::invalid())?;

        Ok(Self(proto))
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        values.extend(HeaderValue::from_str(self.0.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_typed_decode, test_typed_encode};
    use test_case::test_case;

    #[test_case(Scheme::HTTPS, "https")]
    #[test_case(Scheme::HTTP, "http")]
    fn test_encode(scheme: Scheme, expected: &str) -> anyhow::Result<()> {
        let header = XForwardedProto(scheme);

        assert_eq!(test_typed_encode(header), expected);

        Ok(())
    }

    #[test_case("https", Scheme::HTTPS)]
    #[test_case("http", Scheme::HTTP)]
    #[test_case(" http ", Scheme::HTTP; "trims whitespace")]
    fn test_decode(header: &str, expected: Scheme) -> anyhow::Result<()> {
        let decoded = test_typed_decode::<XForwardedProto, _>(header)?.unwrap();

        assert_eq!(decoded.0, expected);

        Ok(())
    }

    #[test_case("" ; "empty")]
    #[test_case(" " ; "single space")]
    #[test_case("   \t  " ; "whitespace only")]
    #[test_case(" , " ; "only empty protos")]
    fn test_decode_empty_or_whitespace_values_are_invalid(header: &str) {
        assert!(test_typed_decode::<XForwardedProto, _>(header).is_err());
    }

    #[test_case("http://"; "invalid proto")]
    #[test_case("https,http://"; "invalid first proto")]
    fn test_decode_invalid_values(header: &str) {
        assert!(test_typed_decode::<XForwardedProto, _>(header).is_err());
    }

    #[test]
    fn accessor_returns_proto() {
        let header = XForwardedProto(Scheme::HTTPS);
        assert_eq!(header.proto().as_str(), "https");
    }
}
