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
        let hosts = value
            .as_bytes()
            .split(|ch| *ch == SEP)
            .map(|hv| hv.trim_ascii())
            .filter(|hv| !hv.is_empty())
            .map(|hv| -> Result<Authority, _> { hv.try_into() })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::invalid())?;

        Ok(Self(hosts))
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        let mut buf = Vec::with_capacity(
            self.0.len() +  // separator
            self.0.iter().map(|host| host.as_str().len()).sum::<usize>(), // hosts
        );

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

    #[test_case(&["docs.rs"], "docs.rs"; "single host")]
    #[test_case(&["crate.docs.rs", "docs.rs"], "crate.docs.rs,docs.rs"; "multiple hosts")]
    #[test_case(&["docs.rs:443", "docs.rs:80"], "docs.rs:443,docs.rs:80"; "hosts with ports")]
    fn test_encode_variations(hosts: &[&'static str], expected: &str) -> anyhow::Result<()> {
        let header = XForwardedHost(
            hosts
                .into_iter()
                .map(|s| Authority::from_static(*s))
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
    #[test_case(" , " ; "only empty hosts")]
    fn test_decode_empty_or_whitespace_values_are_empty(header: &str) -> anyhow::Result<()> {
        let decoded = test_typed_decode::<XForwardedHost, _>(header)?.unwrap();
        assert!(decoded.0.is_empty());

        Ok(())
    }

    #[test_case(",docs.rs", &["docs.rs"]; "ignore empty first host")]
    #[test_case("docs.rs, ", &["docs.rs"]; "ignore empty second host")]
    #[test_case("docs.rs,,crate.docs.rs", &["docs.rs", "crate.docs.rs"]; "ignore empty middle host")]
    fn test_decode_ignores_empty_hosts(header: &str, expected: &[&str]) -> anyhow::Result<()> {
        let decoded = test_typed_decode::<XForwardedHost, _>(header)?.unwrap();
        let decoded = decoded
            .0
            .into_iter()
            .map(|host| host.to_string())
            .collect::<Vec<_>>();
        let expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(decoded, expected);

        Ok(())
    }
}
