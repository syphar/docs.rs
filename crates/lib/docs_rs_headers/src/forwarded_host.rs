use headers::{Error, Header};
use http::{HeaderName, HeaderValue, uri::Authority};

pub static X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

const SEP: u8 = b',';

#[derive(Clone, Debug)]
pub struct XForwardedHost(Vec<Authority>);

impl Header for XForwardedHost {
    fn name() -> &'static HeaderName {
        &X_FORWARDED_HOST
    }

    fn decode<'i, I: Iterator<Item = &'i HeaderValue>>(values: &mut I) -> Result<Self, Error> {
        let Some(value) = values.next() else {
            return Err(Error::invalid());
        };

        // FIXME: unclear: when one host is invalid, should we skip it or fully error out?
        let hosts: Vec<Authority> = value
            .as_bytes()
            .split(|ch| *ch == SEP)
            .map(|hv| -> Result<Authority, _> { hv.trim_ascii().try_into() })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::invalid())?;

        Ok(Self(hosts))
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        let mut buf = Vec::new();

        for host in &self.0 {
            if !buf.is_empty() {
                buf.push(SEP);
            }

            buf.extend_from_slice(host.as_str().as_bytes());
        }

        values.extend(HeaderValue::from_maybe_shared(buf));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_typed_decode, test_typed_encode};
    use test_case::test_case;

    #[test]
    fn test_encode() -> anyhow::Result<()> {
        let header = XForwardedHost(vec![
            Authority::from_static("example.com"),
            Authority::from_static("example.org"),
        ]);

        assert_eq!(test_typed_encode(header), "example.com,example.org");

        Ok(())
    }

    #[test_case(vec!["docs.rs"], "docs.rs"; "single host")]
    #[test_case(vec!["crate.docs.rs", "docs.rs"], "crate.docs.rs,docs.rs"; "multiple hosts")]
    #[test_case(vec!["docs.rs:443", "docs.rs:80"], "docs.rs:443,docs.rs:80"; "hosts with ports")]
    fn test_encode_variations(hosts: Vec<&'static str>, expected: &str) -> anyhow::Result<()> {
        let header = XForwardedHost(
            hosts
                .into_iter()
                .map(Authority::from_static)
                .collect::<Vec<_>>(),
        );

        assert_eq!(test_typed_encode(header), expected);

        Ok(())
    }

    #[test_case("docs.rs", &["docs.rs"]; "single host")]
    #[test_case(
        "crate.docs.rs,docs.rs",
        &["crate.docs.rs", "docs.rs"];
        "multiple hosts no spaces"
    )]
    #[test_case(
        "crate.docs.rs, docs.rs",
        &["crate.docs.rs", "docs.rs"];
        "multiple hosts with spaces"
    )]
    #[test_case(
        "crate.docs.rs:443,docs.rs:80",
        &["crate.docs.rs:443", "docs.rs:80"];
        "multiple hosts with ports"
    )]
    fn test_decode(header: &str, expected: &[&str]) -> anyhow::Result<()> {
        let decoded = test_typed_decode::<XForwardedHost, _>(header)?.unwrap();

        let decoded = decoded
            .0
            .iter()
            .map(|host| host.as_str())
            .collect::<Vec<_>>();

        assert_eq!(decoded, expected);

        Ok(())
    }

    #[test_case("" ; "empty")]
    #[test_case(" " ; "single space")]
    #[test_case("   \t  " ; "whitespace only")]
    #[test_case(", " ; "empty first host")]
    #[test_case("docs.rs, " ; "empty second host")]
    fn test_decode_rejects_empty_or_whitespace_values(header: &str) {
        assert!(test_typed_decode::<XForwardedHost, _>(header).is_err());
    }
}
