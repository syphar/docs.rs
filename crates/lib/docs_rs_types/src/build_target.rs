use serde_with::{DeserializeFromStr, SerializeDisplay};
use std::{fmt, str::FromStr};

include!(concat!(env!("OUT_DIR"), "/static_target_list.rs"));

#[derive(Debug)]
pub struct UnknownBuildTarget(String);

impl core::error::Error for UnknownBuildTarget {}
impl fmt::Display for UnknownBuildTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "unknown build target: {}", self.0)
    }
}

/// validated build target
// #[derive(Debug, Clone, PartialEq, Eq, Hash, DeserializeFromStr, SerializeDisplay)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, DeserializeFromStr, SerializeDisplay)]
pub struct BuildTarget(&'static str);

impl BuildTarget {
    pub fn list() -> impl Iterator<Item = BuildTarget> {
        STATIC_TARGET_LIST.iter().map(|&s| BuildTarget(s))
    }

    pub const fn from_static(s: &'static str) -> Self {
        // TODO: check that the target is valid.
        // let mut i = 0;
        // while i < STATIC_TARGET_LIST.len() {
        //     if STATIC_TARGET_LIST[i].as_bytes() == s.as_bytes() {
        //         return BuildTarget(s);
        //     }
        //     i += 1;
        // }

        // panic!("unknown build target");
        BuildTarget(s)
    }
}

impl AsRef<str> for BuildTarget {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl fmt::Display for BuildTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for BuildTarget {
    type Err = UnknownBuildTarget;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        if let Ok(idx) = STATIC_TARGET_LIST.binary_search(&normalized.as_str()) {
            Ok(Self(STATIC_TARGET_LIST[idx]))
        } else {
            Err(UnknownBuildTarget(s.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test]
    fn test_static_target_list_not_empty() {
        assert!(
            !STATIC_TARGET_LIST.is_empty(),
            "STATIC_TARGET_LIST should not be empty"
        );
        assert!(BuildTarget::list().count() > 0);
    }

    #[test_case("x86_64-unknown-linux-gnu " => "x86_64-unknown-linux-gnu" ; "normal")]
    #[test_case(" x86_64-Unknown-Linux-gnu " => "x86_64-unknown-linux-gnu" ; "trim and lowercase")]
    fn test_parse_ok(input: &'static str) -> &'static str {
        let target: BuildTarget = input.parse().unwrap();
        target.0
    }

    #[test_case("SomeThingElse")]
    fn test_parse_err(input: &'static str) {
        assert!(matches!(
            input.parse::<BuildTarget>().unwrap_err(),
            UnknownBuildTarget(failed_name) if failed_name == input
        ));
    }
}
