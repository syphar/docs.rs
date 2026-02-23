use headers::{Error, Header};
use http::{HeaderName, HeaderValue, uri::Authority};

pub static X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");

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
            .split(|ch| *ch == b',')
            .map(|hv| -> Result<Authority, _> { hv.try_into() })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Error::invalid())?;

        Ok(Self(hosts))
    }

    fn encode<E: Extend<HeaderValue>>(&self, values: &mut E) {
        let mut buf = Vec::new();

        for host in &self.0 {
            if !buf.is_empty() {
                buf.push(b',');
            }

            buf.extend_from_slice(host.as_str().as_bytes());
        }

        values.extend(HeaderValue::from_maybe_shared(buf));
    }
}
