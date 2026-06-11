//! Provides documentation and version information.
//!
//! This Source Code Form is subject to the terms of the Mozilla Public
//! License, v. 2.0. If a copy of the MPL was not distributed with this file,
//! You can obtain one at <http://mozilla.org/MPL/2.0/>.
//!
//! Copyright (c) 2014-2026, Lars Asplund lars.anders.asplund@gmail.com

use core::fmt;
use core::str::FromStr;

/// Current VUnit version string.
pub const VERSION: &str = "5.0.0.dev11";

/// Returns licence text.
#[must_use]
pub const fn license_text() -> &'static str {
    "**VUnit**, except for the projects below, is released under the terms of `Mozilla Public License, v. 2.0`_.\n\
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
.. _SynthWorks Design Inc: http://www.synthworks.com"
}

/// Returns a short introduction to VUnit.
#[must_use]
pub fn doc() -> String {
    const INTRO: &str = "VUnit is an open source unit testing framework for VHDL/SystemVerilog\n\
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
\n";

    let mut text = String::from(INTRO);
    text.push_str(license_text());
    text
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

impl fmt::Display for VUnitVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid version format: {}. Use [v]MAJOR[.MINOR][.PATCH][.devN]",
            self.version_string
        )
    }
}

impl core::error::Error for VUnitVersionError {}

/// VUnit version object which encapsulates knowledge about VUnit versions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct VUnitVersion {
    major: u32,
    minor: u32,
    patch: u32,
    /// Development releases sort before the final release.
    dev: i32,
}

impl VUnitVersion {
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
        let rest = version_string
            .strip_prefix('v')
            .unwrap_or(version_string)
            .trim();

        if rest.is_empty() {
            return Err(VUnitVersionError {
                version_string: version_string.to_owned(),
            });
        }

        let (major, rest) = parse_u32_component(rest).map_err(|_| VUnitVersionError {
            version_string: version_string.to_owned(),
        })?;
        let (minor, rest) = parse_optional_dot_component(rest);
        let (patch, rest) = parse_optional_dot_component(rest);
        let (dev, rest) = parse_optional_dev(rest).map_err(|_| VUnitVersionError {
            version_string: version_string.to_owned(),
        })?;

        if !rest.is_empty() {
            return Err(VUnitVersionError {
                version_string: version_string.to_owned(),
            });
        }

        let dev = dev.map_or(0, |value| i32::try_from(value).unwrap_or(0) - 1000);

        Ok(Self {
            major,
            minor,
            patch,
            dev,
        })
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
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.dev != 0 {
            write!(f, ".dev{}", self.dev + 1000)?;
        }
        Ok(())
    }
}

fn parse_u32_component(s: &str) -> Result<(u32, &str), ()> {
    let end = s.find('.').unwrap_or(s.len());
    let component = &s[..end];
    if component.is_empty() {
        return Err(());
    }
    let value = component.parse().map_err(|_| ())?;
    Ok((value, &s[end..]))
}

fn parse_optional_dot_component(s: &str) -> (u32, &str) {
    if !s.starts_with('.') {
        return (0, s);
    }

    let after_dot = &s[1..];
    let end = after_dot.find('.').unwrap_or(after_dot.len());
    let component = &after_dot[..end];

    component
        .parse::<u32>()
        .map(|value| (value, &after_dot[end..]))
        .unwrap_or((0, s))
}

fn parse_optional_dev(s: &str) -> Result<(Option<u32>, &str), ()> {
    let Some(after_dev) = s.strip_prefix(".dev") else {
        return Ok((None, s));
    };

    let end = after_dev.find('.').unwrap_or(after_dev.len());
    let component = &after_dev[..end];
    if component.is_empty() {
        return Err(());
    }

    let value = component.parse().map_err(|_| ())?;
    Ok((Some(value), &after_dev[end..]))
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
        assert!(VUnitVersion::parse("not-a-version").is_err());
    }
}
