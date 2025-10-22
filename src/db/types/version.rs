use crate::error::Result;
use derive_more::Deref;
use derive_more::Display;
use serde::Serialize;
use sqlx::{
    Postgres,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueRef},
    prelude::*,
};
use std::io::Write;
use std::str::FromStr;

/// NewType around semver::Version to be able to use it with sqlx.
///
/// Represented as string in the database.
#[derive(Debug, Clone, Display, PartialEq, Eq, Hash, Serialize, Deref)]
pub struct Version(pub semver::Version);

impl Type<Postgres> for Version {
    fn type_info() -> PgTypeInfo {
        <String as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <String as Type<Postgres>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Postgres> for Version {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        write!(**buf, "{}", self.0)?;
        Ok(IsNull::No)
    }
}

impl<'r> Decode<'r, Postgres> for Version {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let s: &str = Decode::<Postgres>::decode(value)?;
        Ok(Self(s.parse()?))
    }
}

impl FromStr for Version {
    type Err = semver::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Version(semver::Version::from_str(s)?))
    }
}

impl FromStr for &Version {
    type Err = semver::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(&Version(semver::Version::from_str(s)?))
    }
}
