// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Configurations of testbenches and tests: generics, simulation options, attributes and the
//! VHDL configuration to elaborate.
//!
//! A port of `configuration.py` without `pre_config` and `post_check` hooks. `VUnit` changes
//! configurations through imperative setters; here, [`ConfigurationSet`] applies the
//! declarative [`ConfigurationSpec`]s of a [`ProjectSpec`](crate::spec::ProjectSpec) in order.
//!
//! Differences from `VUnit`:
//!
//! - Generic names are compared case-insensitively, since VHDL identifiers are.
//! - A `fail_on_warning` attribute of a configuration only affects that configuration; `VUnit`
//!   applies it to the whole testbench. `run_all_in_same_sim` is ignored.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use thiserror::Error;

use crate::diagnostics::Diagnostic;
use crate::spec::AssertLevel;
use crate::spec::ConfigurationSpec;
use crate::spec::SimOptions;

/// The attribute that makes warnings stop the simulation.
pub const FAIL_ON_WARNING: &str = "fail_on_warning";

/// The attribute that runs all tests of a testbench in one simulation; ignored.
pub const RUN_ALL_IN_SAME_SIM: &str = "run_all_in_same_sim";

/// Whether `name` is a user attribute (starting with `.`).
pub fn is_user_attribute(name: &str) -> bool {
    name.starts_with('.')
}

/// What a configuration needs to know about its testbench.
#[derive(Debug, Clone, Copy)]
pub struct TestbenchInfo<'a> {
    /// The library name.
    pub library: &'a str,
    /// The entity name.
    pub entity: &'a str,
    /// The lowercase names of the entity's generics.
    pub generic_names: &'a [String],
    /// The file of the entity declaration.
    pub file: &'a Utf8Path,
}

impl TestbenchInfo<'_> {
    /// Whether the entity declares the generic `name` (compared case-insensitively).
    pub fn has_generic(&self, name: &str) -> bool {
        self.generic_names
            .iter()
            .any(|generic| generic.eq_ignore_ascii_case(name))
    }
}

/// A configuration of a testbench or test: one simulation run per test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    /// The name, which becomes part of the testcase names; `None` for the default
    /// configuration.
    pub name: Option<String>,
    /// Generic values.
    pub generics: BTreeMap<String, String>,
    /// Simulation options on top of the project's.
    pub sim_options: SimOptions,
    /// User attributes (names starting with `.`) and their values.
    pub attributes: BTreeMap<String, String>,
    /// A VHDL configuration to elaborate instead of the testbench entity.
    pub vhdl_configuration_name: Option<String>,
    /// The directory of the testbench file.
    pub tb_path: Utf8PathBuf,
}

impl Configuration {
    /// Creates the default configuration of a testbench.
    ///
    /// The `tb_path` generic is set if the entity declares it.
    pub fn default_for(testbench: &TestbenchInfo<'_>) -> Self {
        let tb_path = testbench
            .file
            .parent()
            .map_or_else(Utf8PathBuf::new, Utf8Path::to_owned);
        let mut generics = BTreeMap::new();
        if testbench.has_generic("tb_path") {
            generics.insert("tb_path".to_owned(), directory_generic(&tb_path));
        }
        Self {
            name: None,
            generics,
            sim_options: SimOptions::default(),
            attributes: BTreeMap::new(),
            vhdl_configuration_name: None,
            tb_path,
        }
    }

    /// Whether this is the default configuration.
    pub const fn is_default(&self) -> bool {
        self.name.is_none()
    }

    /// Sets a generic.
    ///
    /// # Errors
    ///
    /// Returns a warning, and leaves the generics unchanged, if the entity doesn't declare it.
    pub fn set_generic(
        &mut self,
        testbench: &TestbenchInfo<'_>,
        name: &str,
        value: &str,
    ) -> Result<(), Diagnostic> {
        if !testbench.has_generic(name) {
            return Err(Diagnostic::warning(format!(
                "generic '{name}' set to value '{value}' not found in entity '{}.{}'; possible \
                 values are [{}]",
                testbench.library,
                testbench.entity,
                testbench.generic_names.join(", ")
            )));
        }
        // A generic set with a different case replaces the existing value.
        self.generics
            .retain(|existing, _| !existing.eq_ignore_ascii_case(name));
        self.generics.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    /// The lowest assertion severity that stops the simulation, if this configuration sets it.
    pub const fn vhdl_assert_stop_level(&self) -> Option<AssertLevel> {
        self.sim_options.vhdl_assert_stop_level
    }
}

/// Formats a directory for a generic: forward slashes and a trailing `/`, as `VUnit` does.
pub fn directory_generic(path: &Utf8Path) -> String {
    let mut value = path.as_str().replace('\\', "/");
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

/// A configuration can't be added.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigurationError {
    /// Configuration names must be non-empty.
    #[error("illegal configuration name '': must be a non-empty string")]
    EmptyName,
    /// Configuration names must be unique per testbench or test.
    #[error("configuration name '{0}' already defined")]
    Duplicate(String),
    /// Attributes of configurations must be user attributes.
    #[error("invalid attribute '{0}': attributes of configurations must start with '.'")]
    InvalidAttribute(String),
}

/// The configurations of a testbench or test, starting with the default configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationSet {
    configurations: Vec<Configuration>,
}

impl ConfigurationSet {
    /// Creates a set with only the default configuration.
    pub fn new(default: Configuration) -> Self {
        Self {
            configurations: vec![default],
        }
    }

    /// All configurations, the default first.
    pub fn all(&self) -> &[Configuration] {
        &self.configurations
    }

    /// The configurations to run: the default configuration is dropped if there are others.
    pub fn to_run(&self) -> &[Configuration] {
        match self.configurations.as_slice() {
            [default] => std::slice::from_ref(default),
            [_, named @ ..] => named,
            [] => &[],
        }
    }

    /// Sets a generic in all configurations.
    ///
    /// # Errors
    ///
    /// Returns a warning, and leaves the generics unchanged, if the entity doesn't declare it.
    pub fn set_generic(
        &mut self,
        testbench: &TestbenchInfo<'_>,
        name: &str,
        value: &str,
    ) -> Result<(), Diagnostic> {
        for configuration in &mut self.configurations {
            configuration.set_generic(testbench, name, value)?;
        }
        Ok(())
    }

    /// Sets simulation options in all configurations.
    pub fn set_sim_options(&mut self, sim_options: &SimOptions) {
        for configuration in &mut self.configurations {
            configuration.sim_options = configuration.sim_options.overridden_by(sim_options);
        }
    }

    /// Sets the VHDL configuration to elaborate in all configurations.
    pub fn set_vhdl_configuration_name(&mut self, name: &str) {
        for configuration in &mut self.configurations {
            configuration.vhdl_configuration_name = Some(name.to_owned());
        }
    }

    /// Adds a configuration that starts as a copy of the default configuration
    /// (`ConfigurationVisitor.add_config`).
    ///
    /// Generics the entity doesn't declare are skipped, and returned as warnings.
    ///
    /// # Errors
    ///
    /// Fails if the name is empty or already used, or if an attribute isn't a user attribute.
    pub fn add(
        &mut self,
        testbench: &TestbenchInfo<'_>,
        spec: &ConfigurationSpec,
    ) -> Result<Vec<Diagnostic>, ConfigurationError> {
        if spec.name.is_empty() {
            return Err(ConfigurationError::EmptyName);
        }
        if self
            .configurations
            .iter()
            .any(|configuration| configuration.name.as_deref() == Some(spec.name.as_str()))
        {
            return Err(ConfigurationError::Duplicate(spec.name.clone()));
        }

        let mut configuration = self.configurations[0].clone();
        configuration.name = Some(spec.name.clone());
        for (name, value) in &spec.attributes {
            match name.as_str() {
                FAIL_ON_WARNING => {
                    // An empty value means the attribute is set without a value, as in a source
                    // comment; only `false` disables it.
                    let level = if value.eq_ignore_ascii_case("false") {
                        AssertLevel::Error
                    } else {
                        AssertLevel::Warning
                    };
                    configuration.sim_options.vhdl_assert_stop_level = Some(level);
                },
                RUN_ALL_IN_SAME_SIM => {},
                _ if is_user_attribute(name) => {
                    configuration.attributes.insert(name.clone(), value.clone());
                },
                _ => return Err(ConfigurationError::InvalidAttribute(name.clone())),
            }
        }
        configuration.sim_options = configuration.sim_options.overridden_by(&spec.sim_options);
        if let Some(name) = &spec.vhdl_configuration_name {
            configuration.vhdl_configuration_name = Some(name.clone());
        }
        let mut warnings = Vec::new();
        for (name, value) in &spec.generics {
            if let Err(warning) = configuration.set_generic(testbench, name, value) {
                warnings.push(warning);
            }
        }
        self.configurations.push(configuration);
        Ok(warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generics(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    fn info<'a>(generic_names: &'a [String], file: &'a str) -> TestbenchInfo<'a> {
        TestbenchInfo {
            library: "lib",
            entity: "tb_entity",
            generic_names,
            file: Utf8Path::new(file),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    #[test]
    fn warning_on_setting_missing_generic() {
        let names = generics(&["runner_cfg"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut configuration = Configuration::default_for(&testbench);
        let warning = configuration
            .set_generic(&testbench, "name123", "value123")
            .unwrap_err();
        for part in ["lib", "tb_entity", "name123", "value123", "runner_cfg"] {
            assert!(warning.message.contains(part), "{}", warning.message);
        }
        assert!(configuration.generics.is_empty());
    }

    #[test]
    fn generics_are_case_insensitive() {
        let names = generics(&["runner_cfg", "value"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut configuration = Configuration::default_for(&testbench);
        configuration.set_generic(&testbench, "value", "1").unwrap();
        configuration.set_generic(&testbench, "VALUE", "2").unwrap();
        assert_eq!(configuration.generics, map(&[("VALUE", "2")]));
    }

    #[test]
    fn does_not_add_tb_path_generic() {
        let names = generics(&["runner_cfg"]);
        let configuration = Configuration::default_for(&info(&names, "/tb/file.vhd"));
        assert!(configuration.generics.is_empty());
        assert_eq!(configuration.tb_path, "/tb");
    }

    #[test]
    fn adds_tb_path_generic() {
        let names = generics(&["runner_cfg", "tb_path"]);
        let configuration = Configuration::default_for(&info(&names, "/some/other_path/file.vhd"));
        assert_eq!(
            configuration.generics,
            map(&[("tb_path", "/some/other_path/")])
        );
    }

    #[test]
    fn directory_generic_uses_forward_slashes() {
        assert_eq!(directory_generic(Utf8Path::new(r"C:\tb\dir")), "C:/tb/dir/");
        assert_eq!(directory_generic(Utf8Path::new("/")), "/");
    }

    #[test]
    fn default_configuration_has_no_attributes() {
        let names = generics(&["runner_cfg"]);
        let configuration = Configuration::default_for(&info(&names, "/tb/file.vhd"));
        assert!(configuration.attributes.is_empty());
        assert!(configuration.is_default());
    }

    #[test]
    fn add_copies_the_default_configuration() {
        let names = generics(&["runner_cfg", "value", "global_value"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut set = ConfigurationSet::new(Configuration::default_for(&testbench));
        set.set_generic(&testbench, "global_value", "global value")
            .unwrap();
        assert_eq!(set.to_run().len(), 1);
        assert!(set.to_run()[0].is_default());

        set.add(
            &testbench,
            &ConfigurationSpec {
                name: "value=1".to_owned(),
                generics: map(&[("value", "1"), ("global_value", "local value")]),
                ..ConfigurationSpec::default()
            },
        )
        .unwrap();
        set.add(
            &testbench,
            &ConfigurationSpec {
                name: "value=2".to_owned(),
                generics: map(&[("value", "2")]),
                attributes: map(&[(".foo", "bar")]),
                vhdl_configuration_name: Some("cfg".to_owned()),
                ..ConfigurationSpec::default()
            },
        )
        .unwrap();

        let to_run = set.to_run();
        assert_eq!(to_run.len(), 2);
        assert_eq!(to_run[0].name.as_deref(), Some("value=1"));
        assert_eq!(
            to_run[0].generics,
            map(&[("global_value", "local value"), ("value", "1")])
        );
        assert!(to_run[0].attributes.is_empty());
        assert_eq!(to_run[0].vhdl_configuration_name, None);
        assert_eq!(
            to_run[1].generics,
            map(&[("global_value", "global value"), ("value", "2")])
        );
        assert_eq!(to_run[1].attributes, map(&[(".foo", "bar")]));
        assert_eq!(to_run[1].vhdl_configuration_name.as_deref(), Some("cfg"));
    }

    #[test]
    fn add_rejects_invalid_configurations() {
        let names = generics(&["runner_cfg"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut set = ConfigurationSet::new(Configuration::default_for(&testbench));
        let spec = |name: &str| ConfigurationSpec {
            name: name.to_owned(),
            ..ConfigurationSpec::default()
        };
        assert_eq!(
            set.add(&testbench, &spec("")),
            Err(ConfigurationError::EmptyName)
        );
        set.add(&testbench, &spec("c1")).unwrap();
        assert_eq!(
            set.add(&testbench, &spec("c1")),
            Err(ConfigurationError::Duplicate("c1".to_owned()))
        );
        assert_eq!(
            set.add(
                &testbench,
                &ConfigurationSpec {
                    attributes: map(&[("foo", "bar")]),
                    ..spec("c3")
                }
            ),
            Err(ConfigurationError::InvalidAttribute("foo".to_owned()))
        );
        assert_eq!(set.all().len(), 2);
    }

    #[test]
    fn add_warns_about_unknown_generics() {
        let names = generics(&["runner_cfg"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut set = ConfigurationSet::new(Configuration::default_for(&testbench));
        let warnings = set
            .add(
                &testbench,
                &ConfigurationSpec {
                    name: "c".to_owned(),
                    generics: map(&[("missing", "1")]),
                    ..ConfigurationSpec::default()
                },
            )
            .unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(set.all()[1].generics.is_empty());
    }

    #[test]
    fn sim_options_and_builtin_attributes() {
        let names = generics(&["runner_cfg"]);
        let testbench = info(&names, "/tb/file.vhd");
        let mut set = ConfigurationSet::new(Configuration::default_for(&testbench));
        set.set_sim_options(&SimOptions {
            disable_ieee_warnings: Some(true),
            ..SimOptions::default()
        });
        set.add(
            &testbench,
            &ConfigurationSpec {
                name: "c1".to_owned(),
                sim_options: SimOptions {
                    disable_ieee_warnings: Some(false),
                    ..SimOptions::default()
                },
                attributes: map(&[(FAIL_ON_WARNING, ""), (RUN_ALL_IN_SAME_SIM, "")]),
                ..ConfigurationSpec::default()
            },
        )
        .unwrap();
        set.add(
            &testbench,
            &ConfigurationSpec {
                name: "c2".to_owned(),
                ..ConfigurationSpec::default()
            },
        )
        .unwrap();
        set.add(
            &testbench,
            &ConfigurationSpec {
                name: "c3".to_owned(),
                attributes: map(&[(FAIL_ON_WARNING, "False")]),
                ..ConfigurationSpec::default()
            },
        )
        .unwrap();
        let [c1, c2, c3] = set.to_run() else {
            panic!("expected three configurations");
        };
        assert_eq!(c1.sim_options.disable_ieee_warnings, Some(false));
        assert_eq!(c1.vhdl_assert_stop_level(), Some(AssertLevel::Warning));
        assert!(c1.attributes.is_empty());
        assert_eq!(c2.sim_options.disable_ieee_warnings, Some(true));
        assert_eq!(c2.vhdl_assert_stop_level(), None);
        assert_eq!(c3.vhdl_assert_stop_level(), Some(AssertLevel::Error));
    }
}
