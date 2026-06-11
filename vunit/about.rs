//! Provides documentation and version information.
//!
//! This Source Code Form is subject to the terms of the Mozilla Public
//! License, v. 2.0. If a copy of the MPL was not distributed with this file,
//! You can obtain one at <http://mozilla.org/MPL/2.0/>.
//!
//! Copyright (c) 2014-2026, Lars Asplund lars.anders.asplund@gmail.com

use core::cmp::Ordering;
use core::error::Error;
use core::fmt;
use core::str::FromStr;

/// Current VUnit version string.
pub const VERSION: &str = "5.0.0.dev11";

/// Returns license text.
#[must_use]
pub const fn license_text() -> &'static str {
    "\
**VUnit**, except for the projects below, is released under the terms of `Mozilla Public License, v. 2.0`_.\n\
|copy| 2014-2024 Lars Asplund, lars.anders.asplund@gmail.com.\n\
\n\
The following library is `redistributed`_ with VUnit for convenience:\n\
\n\
* **OSVVM** (``vunit/vhdl/osvvm``): these files are licensed under the terms of `Apache License, v 2.0`_,\n\
  |copy| 2010 - 2023 by `SynthWorks Design Inc`_. All rights reserved.\n\
\n\
The font used in VUnit's logo and illustrations is 'Tratex', the traffic sign typeface used on swedish road signs:\n\
\n\
- `transportstyrelsen.se: Teckensnitt <https://transportstyrelsen.se/sv/vagtrafik/Trafikregler/Om-vagmarken/Teckensnitt/>`__\n\
- `Wikipedia: Tratex <https://en.wikipedia.org/wiki/Tratex>`__\n\
\n\
\n\
.. |copy|   unicode:: U+000A9 .. COPYRIGHT SIGN\n\
.. _redistributed: https://github.com/VUnit/vunit/blob/master/.gitmodules\n\
.. _Mozilla Public License, v. 2.0: http://mozilla.org/MPL/2.0/\n\
.. _ARTISTIC License: http://www.perlfoundation.org/artistic_license_2_0\n\
.. _Apache License, v 2.0: http://www.apache.org/licenses/LICENSE-2.0\n\
.. _SynthWorks Design Inc: http://www.synthworks.com\n\
"
}

/// Returns a short introduction to VUnit.
#[must_use]
pub fn doc() -> String {
    const INTRO: &str = "\
VUnit is an open source unit testing framework for VHDL/SystemVerilog\n\
released under the terms of Mozilla Public License, v. 2.0. It\n\
features the functionality needed to realize continuous and automated\n\
testing of your HDL code. VUnit doesn't replace but rather complements\n\
traditional testing methodologies by supporting a \"test early and\n\
often\" approach through automation. **Read more on our**\n\
`Website <https://vunit.github.io>`__\n\
\n\
Contributing in the form of code, feedback, ideas or bug reports are\n\
welcome. Read our `contribution guide\n\
<https://vunit.github.io/contributing.html>`__ to get started.\n\
\n\
";

    format!("{INTRO}{}", license_text())
}

/// Returns the VUnit version.
#[must_use]
pub const fn version() -> &'static str {
    VERSION
}

/// Error returned when a version string cannot be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VUnitVersionError {
    version_string: String,
}

impl From<&str> for VUnitVersionError {
    fn from(value: &str) -> Self {
        Self {
            version_string: value.to_owned(),
        }
    }
}

impl fmt::Display for VUnitVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid version format: {}. Use [v]MAJOR[.MINOR][.PATCH][.devN]",
            self.version_string
        )
    }
}

impl Error for VUnitVersionError {}

/// VUnit version object which encapsulates knowledge about VUnit versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VUnitVersion {
    major: u32,
    minor: u32,
    patch: u32,
    /// Development releases sort before the final release.
    dev: Option<u32>,
}

impl PartialOrd for VUnitVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VUnitVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            // make sure that dev versions are sorted before non-dev versions
            .then_with(|| match (self.dev, other.dev) {
                (Some(self_dev), Some(other_dev)) => self_dev.cmp(&other_dev),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    }
}

impl VUnitVersion {
    const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            dev: None,
        }
    }

    const fn new_dev(major: u32, minor: u32, patch: u32, dev: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            dev: Some(dev),
        }
    }

    /// Parses a VUnit version string.
    ///
    /// Not a full implementation of PEP 440, only what is needed for VUnit versioning.
    ///
    /// # Errors
    ///
    /// Returns [`VUnitVersionError`] if `version_string` does not match
    /// `[v]MAJOR[.MINOR][.PATCH][.devN]`.
    pub fn parse(version_string: &str) -> Result<Self, VUnitVersionError> {
        let version_string = version_string.trim();
        let rest = version_string.strip_prefix('v').unwrap_or(version_string);

        let mut parts = rest.split('.');

        let major = parts
            .next()
            .ok_or(version_string)?
            .parse::<u32>()
            .map_err(|_error| version_string)?;

        let parse_num = |part: &str| part.parse::<u32>().map_err(|_error| version_string);
        let parse_dev = |part: &str| -> Result<_, VUnitVersionError> {
            part.strip_prefix("dev")
                .map(str::parse::<u32>)
                .transpose()
                .map_err(|_error| version_string.into())
        };

        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(minor), Some(patch), Some(dev), None) => {
                let minor = parse_num(minor)?;
                let patch = parse_num(patch)?;
                let dev = parse_dev(dev)?.ok_or(version_string)?;
                Ok(Self::new_dev(major, minor, patch, dev))
            }
            (Some(minor), Some(patch_or_dev), None, _) => {
                let minor = parse_num(minor)?;
                if let Some(dev) = parse_dev(patch_or_dev)? {
                    Ok(Self::new_dev(major, minor, 0, dev))
                } else {
                    let patch = parse_num(patch_or_dev)?;
                    Ok(Self::new(major, minor, patch))
                }
            }
            (Some(minor_or_dev), None, _, _) => {
                if let Some(dev) = parse_dev(minor_or_dev)? {
                    Ok(Self::new_dev(major, 0, 0, dev))
                } else {
                    let minor = parse_num(minor_or_dev)?;
                    Ok(Self::new(major, minor, 0))
                }
            }
            (None, _, _, _) => Ok(Self::new(major, 0, 0)),
            (Some(_), Some(_), Some(_), Some(_)) => Err(version_string.into()),
        }
    }
}

impl FromStr for VUnitVersion {
    type Err = VUnitVersionError;

    fn from_str(version_string: &str) -> Result<Self, Self::Err> {
        Self::parse(version_string)
    }
}

impl fmt::Display for VUnitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            major,
            minor,
            patch,
            dev,
        } = self;
        if let Some(dev) = dev {
            write!(f, "{major}.{minor}.{patch}.dev{dev}")
        } else {
            write!(f, "{major}.{minor}.{patch}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VERSION, VUnitVersion, doc, license_text, version};

    #[test]
    fn version_matches_constant() {
        assert_eq!(version(), VERSION);
    }

    #[test]
    fn doc_includes_license_text() {
        let documentation = doc();
        assert!(documentation.contains("VUnit is an open source unit testing framework"));
        assert!(documentation.contains(license_text()));
    }

    #[test]
    fn parse_release_version() {
        let parsed = VUnitVersion::parse("5.0.0").unwrap();
        assert_eq!(parsed.to_string(), "5.0.0");
    }

    #[test]
    fn parse_prefixed_dev_version() {
        let parsed = VUnitVersion::parse("v5.0.0.dev11").unwrap();
        assert_eq!(parsed.to_string(), "5.0.0.dev11");
    }

    #[test]
    fn parse_short_dev_version() {
        let parsed = VUnitVersion::parse("5.dev11").unwrap();
        assert_eq!(parsed.to_string(), "5.0.0.dev11");
    }

    #[test]
    fn dev_versions_sort_before_release() {
        let dev = VUnitVersion::parse("5.0.0.dev11").unwrap();
        let release = VUnitVersion::parse("5.0.0").unwrap();
        assert!(dev < release);
    }

    #[test]
    fn rejects_invalid_version() {
        VUnitVersion::parse("not-a-version").unwrap_err();
    }
}
