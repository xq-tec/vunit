// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! VHDL standard revisions, and the standard tags in builtin file names.
//!
//! Ported from `vhdl_standard.py` and the file-name filtering in `builtins.py`.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use thiserror::Error;

/// A VHDL standard revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum VhdlStandard {
    /// IEEE 1076-1993.
    Vhdl1993,
    /// IEEE 1076-2002.
    Vhdl2002,
    /// IEEE 1076-2008, the default.
    #[default]
    Vhdl2008,
    /// IEEE 1076-2019.
    Vhdl2019,
}

/// A string that doesn't name a VHDL standard.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown VHDL standard '{0}'")]
pub struct UnknownVhdlStandard(pub String);

impl VhdlStandard {
    /// All standards, oldest first.
    pub const ALL: [Self; 4] = [
        Self::Vhdl1993,
        Self::Vhdl2002,
        Self::Vhdl2008,
        Self::Vhdl2019,
    ];

    /// The four-digit year of the standard.
    pub const fn year(self) -> u16 {
        match self {
            Self::Vhdl1993 => 1993,
            Self::Vhdl2002 => 2002,
            Self::Vhdl2008 => 2008,
            Self::Vhdl2019 => 2019,
        }
    }

    /// The name `VUnit` uses: `93` for VHDL-93 (for legacy reasons), the year otherwise.
    ///
    /// This is also the tag that builtin file names carry.
    pub const fn vunit_name(self) -> &'static str {
        match self {
            Self::Vhdl1993 => "93",
            Self::Vhdl2002 => "2002",
            Self::Vhdl2008 => "2008",
            Self::Vhdl2019 => "2019",
        }
    }

    /// Whether the standard has context declarations.
    pub fn supports_context(self) -> bool {
        self >= Self::Vhdl2008
    }

    /// Whether a builtin file named `file_name` can be compiled with this standard.
    ///
    /// A name containing `<tag>p` supports that standard and later ones, `<tag>m` that
    /// standard and earlier ones, and a plain `<tag>` only that standard. Names without
    /// tags support every standard.
    pub fn is_allowed_by_file_name(self, file_name: &str) -> bool {
        let mut any_tag = false;
        let mut allowed = false;
        for standard in Self::ALL {
            let tag = standard.vunit_name();
            let supports = if file_name.contains(&format!("{tag}p")) {
                self >= standard
            } else if file_name.contains(&format!("{tag}m")) {
                self <= standard
            } else if file_name.contains(tag) {
                self == standard
            } else {
                continue;
            };
            any_tag = true;
            allowed |= supports;
        }
        !any_tag || allowed
    }
}

impl fmt::Display for VhdlStandard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.vunit_name())
    }
}

impl FromStr for VhdlStandard {
    type Err = UnknownVhdlStandard;

    /// Accepts the year (`2008`) or its last two digits (`08`).
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|standard| {
                let year = standard.year().to_string();
                name == year || (name.len() == 2 && year.ends_with(name))
            })
            .ok_or_else(|| UnknownVhdlStandard(name.to_owned()))
    }
}

impl Serialize for VhdlStandard {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.year().to_string())
    }
}

impl<'de> Deserialize<'de> for VhdlStandard {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vhdl_standard::VhdlStandard::*;

    #[test]
    fn valid_standards() {
        for (name, standard) in [
            ("93", Vhdl1993),
            ("02", Vhdl2002),
            ("08", Vhdl2008),
            ("19", Vhdl2019),
            ("1993", Vhdl1993),
            ("2002", Vhdl2002),
            ("2008", Vhdl2008),
            ("2019", Vhdl2019),
        ] {
            assert_eq!(name.parse(), Ok(standard));
        }
    }

    #[test]
    fn error_on_invalid_standard() {
        for name in ["2001", "002", "993", "2", "3"] {
            assert_eq!(
                name.parse::<VhdlStandard>(),
                Err(UnknownVhdlStandard(name.to_owned()))
            );
        }
    }

    #[test]
    fn comparison() {
        assert!(Vhdl1993 < Vhdl2002);
        assert!(Vhdl2002 < Vhdl2008);
        assert!(Vhdl2008 < Vhdl2019);
    }

    #[test]
    fn display() {
        assert_eq!(Vhdl1993.to_string(), "93");
        assert_eq!(Vhdl2002.to_string(), "2002");
    }

    #[test]
    fn supports_context() {
        assert!(!Vhdl2002.supports_context());
        assert!(Vhdl2008.supports_context());
    }

    #[test]
    fn file_name_tags() {
        // `and_later` and `and_earlier` in `vhdl_standard.py`.
        let allowed = |file_name: &str| -> Vec<VhdlStandard> {
            VhdlStandard::ALL
                .into_iter()
                .filter(|standard| standard.is_allowed_by_file_name(file_name))
                .collect()
        };
        assert_eq!(allowed("foo.vhd"), VhdlStandard::ALL);
        assert_eq!(allowed("foo-2008p.vhd"), [Vhdl2008, Vhdl2019]);
        assert_eq!(allowed("foo-2008m.vhd"), [Vhdl1993, Vhdl2002, Vhdl2008]);
        assert_eq!(allowed("foo-93.vhd"), [Vhdl1993]);
        assert_eq!(allowed("foo-2002p.vhd"), [Vhdl2002, Vhdl2008, Vhdl2019]);
        assert_eq!(allowed("location_pkg-body-2019p.vhd"), [Vhdl2019]);
    }

    #[test]
    fn serde_uses_year() {
        assert_eq!(serde_json::to_string(&Vhdl1993).unwrap(), "\"1993\"");
        assert_eq!(
            serde_json::from_str::<VhdlStandard>("\"08\"").unwrap(),
            Vhdl2008
        );
    }
}
