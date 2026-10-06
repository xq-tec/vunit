// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Loading the project of a workspace: reading the configuration, collecting and parsing the
//! sources, and discovering the testcases.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::sync::Arc;

use camino::Utf8Path;
use camino::Utf8PathBuf;

use crate::config;
use crate::diagnostics::Diagnostic;
use crate::diagnostics::error_chain;
use crate::discovery;
use crate::discovery::Discovery;
use crate::project::Project;
use crate::sources;
use crate::sources::SourceCache;
use crate::spec::ProjectSpec;
use crate::store::OutputLayout;
use crate::watch;
use crate::watch::WatchTargets;
use crate::workspace::ProjectSource;

/// A loaded project.
#[derive(Debug)]
pub(super) struct Model {
    /// The specification the project was built from.
    pub spec: ProjectSpec,
    /// The project.
    pub project: Project,
    /// Its testbenches and testcases.
    pub discovery: Discovery,
}

/// The result of loading the project.
#[derive(Debug)]
pub(super) struct Loaded {
    /// The project, or `None` if the configuration never had a valid specification.
    pub model: Option<Arc<Model>>,
    /// Problems with the configuration file, the specification and the test configurations.
    pub config_diagnostics: Vec<Diagnostic>,
    /// Problems with source files and testbenches.
    pub project_diagnostics: Vec<Diagnostic>,
    /// The directories to watch.
    pub watch_targets: WatchTargets,
}

/// The configuration of a project: where it comes from, and the last valid specification.
#[derive(Debug)]
pub(super) struct ProjectConfig {
    config_file: Option<Utf8PathBuf>,
    /// The last valid specification.
    spec: Option<ProjectSpec>,
    /// The problems found when the configuration file was last read.
    config_file_diagnostics: Vec<Diagnostic>,
}

impl ProjectConfig {
    /// Creates a configuration from `source`.
    ///
    /// The configuration file path, if any, is absolute. A configuration file isn't read yet.
    pub(super) fn new(source: ProjectSource) -> Self {
        let (config_file, spec) = match source {
            ProjectSource::ConfigFile(path) => (Some(path), None),
            ProjectSource::Spec(spec) => (None, Some(spec)),
        };
        Self {
            config_file,
            spec,
            config_file_diagnostics: Vec::new(),
        }
    }

    /// Reads the configuration file, if there is one.
    ///
    /// A valid specification replaces the previous one; otherwise the previous one stays.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be read; the problem is also kept as a diagnostic.
    pub(super) fn read(&mut self) -> Result<(), config::ReadError> {
        let Some(path) = &self.config_file else {
            return Ok(());
        };
        match config::load(path) {
            Ok(config) => {
                if let Some(spec) = config.spec {
                    self.spec = Some(spec);
                }
                self.config_file_diagnostics = config.diagnostics;
                Ok(())
            },
            Err(error) => {
                self.config_file_diagnostics =
                    vec![Diagnostic::error(error_chain(&error)).in_file(path)];
                Err(error)
            },
        }
    }
}

/// Loads the project of a workspace again and again, keeping the parse results of unchanged
/// files.
#[derive(Debug)]
pub(super) struct Loader {
    root: Utf8PathBuf,
    layout: OutputLayout,
    /// The directory with the extracted builtins.
    builtins_dir: Utf8PathBuf,
    config: ProjectConfig,
    cache: SourceCache,
}

impl Loader {
    /// A loader for the project of `config`, starting with the parse results in `cache`.
    pub(super) const fn new(
        root: Utf8PathBuf,
        layout: OutputLayout,
        config: ProjectConfig,
        builtins_dir: Utf8PathBuf,
        cache: SourceCache,
    ) -> Self {
        Self {
            root,
            layout,
            builtins_dir,
            config,
            cache,
        }
    }

    /// The configuration file, if the project comes from one.
    pub(super) fn config_file(&self) -> Option<&Utf8Path> {
        self.config.config_file.as_deref()
    }

    /// Loads the project, reading the configuration file again first if `read_config`.
    pub(super) fn load(&mut self, read_config: bool) -> Loaded {
        if read_config && let Err(error) = self.config.read() {
            tracing::debug!(
                error = error_chain(&error),
                "failed to read the configuration"
            );
        }
        let config = &self.config;
        let empty = ProjectSpec::new();
        let watch_targets = watch::watch_targets(
            &self.root,
            self.layout.root(),
            config.config_file.as_deref(),
            config.spec.as_ref().unwrap_or(&empty),
        );
        let mut config_diagnostics = config.config_file_diagnostics.clone();
        let Some(spec) = &config.spec else {
            return Loaded {
                model: None,
                config_diagnostics,
                project_diagnostics: Vec::new(),
                watch_targets,
            };
        };
        let loaded = sources::build_project(&self.root, spec, &self.builtins_dir, &mut self.cache);
        let discovery = discovery::discover(&loaded.project, &spec.test_configs);
        config_diagnostics.extend(loaded.config_diagnostics);
        config_diagnostics.extend(discovery.config_diagnostics.iter().cloned());
        let mut project_diagnostics = loaded.project_diagnostics;
        project_diagnostics.extend(discovery.project_diagnostics.iter().cloned());
        Loaded {
            model: Some(Arc::new(Model {
                spec: spec.clone(),
                project: loaded.project,
                discovery,
            })),
            config_diagnostics,
            project_diagnostics,
            watch_targets,
        }
    }

    /// Writes the parse cache.
    pub(super) fn save_cache(&self) {
        let path = self.layout.parse_cache_file();
        if let Err(error) = self.cache.save(&path) {
            tracing::warn!(%path, %error, "failed to save the parse cache");
        }
    }
}
