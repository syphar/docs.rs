use anyhow::Result;
use serde::Serialize;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Default)]
pub enum Via {
    #[default]
    ApexDomain,
    SubDomain,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid via format: {0}")]
pub struct InvalidVia(String);

impl FromStr for Via {
    type Err = InvalidVia;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("apex_domain") {
            Ok(Self::ApexDomain)
        } else if s.eq_ignore_ascii_case("sub_domain") {
            Ok(Self::SubDomain)
        } else {
            Err(InvalidVia(s.to_string()))
        }
    }
}
