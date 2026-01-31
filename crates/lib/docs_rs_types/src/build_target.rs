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
///
/// Target list is based on the output of `rustc --print target-list` at
/// compile time, so only contains the currently supported targets.
///
/// Missing might be legacy targets, or custom json-file targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, DeserializeFromStr, SerializeDisplay)]
pub struct BuildTarget(&'static str);

impl BuildTarget {
    pub fn list() -> impl Iterator<Item = BuildTarget> {
        STATIC_TARGET_LIST.iter().map(|&s| BuildTarget(s))
    }

    fn find(s: &str) -> Option<usize> {
        let normalized = s.trim().to_lowercase();
        STATIC_TARGET_LIST.binary_search(&normalized.as_str()).ok()
    }

    pub fn exists(s: &str) -> bool {
        Self::find(s).is_some()
    }

    pub const fn from_static(s: &'static str) -> Self {
        let mut i = 0;
        while i < STATIC_TARGET_LIST.len() {
            if str_eq(STATIC_TARGET_LIST[i], s) {
                return BuildTarget(s);
            }
            i += 1;
        }

        panic!("unknown build target");
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut i = 0;
    while i < a_bytes.len() {
        if a_bytes[i] != b_bytes[i] {
            return false;
        }
        i += 1;
    }
    true
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
        if let Some(idx) = Self::find(s) {
            Ok(Self(STATIC_TARGET_LIST[idx]))
        } else {
            Err(UnknownBuildTarget(s.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::{
        collections::BTreeSet,
        fs,
        io::{self, BufRead as _},
    };
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

    #[test]
    fn test_validate() -> Result<()> {
        let mut invalid_targets: BTreeSet<(String, String, String)> = BTreeSet::new();
        for line in io::BufReader::new(fs::File::open(
            "/Users/syphar/Dropbox/rust-lang/docs-rs/build_targets/targets.csv",
        )?)
        .lines()
        .skip(1)
        {
            let line = line?;
            let line: Vec<_> = line.split(',').collect();
            // name,version,id,default_target,doc_targets

            let name = line[0];
            let version = line[1];
            let _id = line[2];
            let default = line[3].replace("\"", "");
            let rest = line[4];

            if !default.is_empty() && default.parse::<BuildTarget>().is_err() {
                invalid_targets.insert((default, name.into(), version.into()));
            }

            let mut rest = rest.to_string();
            for ch in &['\"', '[', ']'] {
                rest = rest.replace(*ch, "");
            }
            for t in rest.split(',') {
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                if t.parse::<BuildTarget>().is_err() {
                    invalid_targets.insert((t.to_string(), name.into(), version.into()));
                }
            }
        }
        if !invalid_targets.is_empty() {
            panic!("some targets are invalid: {:?}", invalid_targets);
        }
        Ok(())
    }
}
