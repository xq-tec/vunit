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
use crate::discovery;
use crate::discovery::Discovery;
use crate::project::Project;
use crate::sources;
use crate::sources::SourceCache;
use crate::spec::ProjectSpec;
use crate::store::OutputLayout;
use crate::watch;
use crate::watch::WatchTargets;

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

/// Loads the project of a workspace again and again, keeping the parse results of unchanged
/// files.
#[derive(Debug)]
pub(super) struct Loader {
    root: Utf8PathBuf,
    layout: OutputLayout,
    builtins_dir: Utf8PathBuf,
    config_file: Option<Utf8PathBuf>,
    /// The last valid specification.
    spec: Option<ProjectSpec>,
    /// The problems found when the configuration file was last read.
    config_file_diagnostics: Vec<Diagnostic>,
    cache: SourceCache,
}

impl Loader {
    /// A loader for a project from `config_file`, or for the fixed `spec`.
    pub(super) fn new(
        root: Utf8PathBuf,
        layout: OutputLayout,
        config_file: Option<Utf8PathBuf>,
        spec: Option<ProjectSpec>,
    ) -> Self {
        Self {
            root,
            layout,
            builtins_dir: Utf8PathBuf::new(),
            config_file,
            spec,
            config_file_diagnostics: Vec::new(),
            cache: SourceCache::new(),
        }
    }

    /// The configuration file, if the project comes from one.
    pub(super) fn config_file(&self) -> Option<&Utf8Path> {
        self.config_file.as_deref()
    }

    /// Sets the directory with the extracted builtins and the persisted parse cache.
    pub(super) fn prepare(&mut self, builtins_dir: Utf8PathBuf, cache: SourceCache) {
        self.builtins_dir = builtins_dir;
        self.cache = cache;
    }

    /// Reads the configuration file, if there is one. A valid specification replaces the
    /// previous one; otherwise the previous one stays.
    ///
    /// # Errors
    ///
    /// Fails if the file can't be read; the problem is also kept as a diagnostic.
    pub(super) fn read_config(&mut self) -> Result<(), config::ReadError> {
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
                    vec![Diagnostic::error(format!("{error}: {}", error.source)).in_file(path)];
                Err(error)
            },
        }
    }

    /// Loads the project, reading the configuration file again first if `read_config`.
    pub(super) fn load(&mut self, read_config: bool) -> Loaded {
        if read_config && let Err(error) = self.read_config() {
            tracing::debug!(%error, "failed to read the configuration");
        }
        let mut config_diagnostics = self.config_file_diagnostics.clone();
        let Some(spec) = &self.spec else {
            return Loaded {
                model: None,
                config_diagnostics,
                project_diagnostics: Vec::new(),
                watch_targets: watch::watch_targets(
                    &self.root,
                    self.layout.root(),
                    self.config_file.as_deref(),
                    &ProjectSpec::new(),
                ),
            };
        };
        let loaded = sources::build_project(&self.root, spec, &self.builtins_dir, &mut self.cache);
        let discovery = discovery::discover(&loaded.project, &spec.test_configs);
        config_diagnostics.extend(loaded.config_diagnostics);
        config_diagnostics.extend(discovery.config_diagnostics.iter().cloned());
        let mut project_diagnostics = loaded.project_diagnostics;
        project_diagnostics.extend(discovery.project_diagnostics.iter().cloned());
        let watch_targets = watch::watch_targets(
            &self.root,
            self.layout.root(),
            self.config_file.as_deref(),
            spec,
        );
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
