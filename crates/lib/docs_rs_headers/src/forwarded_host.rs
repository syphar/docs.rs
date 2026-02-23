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
            .map(|hv| -> Result<Authority, _> { hv.try_into() })
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
    use std::ops::RangeInclusive;
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

    // #[test]
    // fn test_decode() -> anyhow::Result<()> {
    //     assert_eq!(
    //         test_typed_decode::<SurrogateKeys, _>("key-1 key-2 key-2")?.unwrap(),
    //         SurrogateKeys::from_iter_until_full([
    //             SurrogateKey::from_str("key-2").unwrap(),
    //             SurrogateKey::from_str("key-1").unwrap(),
    //         ]),
    //     );

    //     Ok(())
    // }
}
