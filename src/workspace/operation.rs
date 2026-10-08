// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Compile and simulate operations, and how queued requests merge.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::num::NonZeroUsize;

use super::RequestTag;
use crate::runner::SimulationRequest;

/// A requested operation.
///
/// A simulate operation compiles first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Operation {
    /// Compile the testbenches.
    Compile {
        /// The tags of the merged requests.
        tags: Vec<RequestTag>,
        /// The maximum number of processes of the operation at once; `None` for no own limit.
        max_parallel: Option<NonZeroUsize>,
    },
    /// Compile the testbenches, then run the testcases matching `requests`.
    Simulate {
        /// The merged requests, one per pattern.
        requests: Vec<SimulationRequest>,
        /// The tags of the merged requests.
        tags: Vec<RequestTag>,
        /// The maximum number of processes of the operation at once; `None` for no own limit.
        max_parallel: Option<NonZeroUsize>,
    },
}

impl Operation {
    /// A compile operation with the tag and process limit of its request.
    pub(super) fn compile(tag: Option<RequestTag>, max_parallel: Option<NonZeroUsize>) -> Self {
        Self::Compile {
            tags: tag.into_iter().collect(),
            max_parallel,
        }
    }

    /// A simulate operation with the tag and process limit of its request.
    pub(super) fn simulate(
        requests: Vec<SimulationRequest>,
        tag: Option<RequestTag>,
        max_parallel: Option<NonZeroUsize>,
    ) -> Self {
        Self::Simulate {
            requests: merge_requests(Vec::new(), requests),
            tags: tag.into_iter().collect(),
            max_parallel,
        }
    }

    /// The tags of the merged requests.
    pub(super) fn tags(&self) -> &[RequestTag] {
        match self {
            Self::Compile { tags, .. } | Self::Simulate { tags, .. } => tags,
        }
    }

    /// The maximum number of processes of the operation at once; `None` for no own limit.
    pub(super) const fn max_parallel(&self) -> Option<NonZeroUsize> {
        match self {
            Self::Compile { max_parallel, .. } | Self::Simulate { max_parallel, .. } => {
                *max_parallel
            },
        }
    }

    /// Removes `tag` from the merged requests' tags.
    ///
    /// The requests of a simulate stay: they can't be told apart once merged, and the other
    /// requests may have asked for the same testcases.
    pub(super) fn remove_tag(&mut self, tag: &RequestTag) {
        match self {
            Self::Compile { tags, .. } | Self::Simulate { tags, .. } => {
                tags.retain(|other| other != tag);
            },
        }
    }

    /// The patterns of a simulate operation; `None` for a compile operation.
    pub(super) fn simulation_patterns(&self) -> Option<Vec<String>> {
        match self {
            Self::Compile { .. } => None,
            Self::Simulate { requests, .. } => Some(
                requests
                    .iter()
                    .map(|request| request.pattern.clone())
                    .collect(),
            ),
        }
    }

    /// The requests of a simulate operation, the tags, and the process limit.
    fn into_parts(
        self,
    ) -> (
        Option<Vec<SimulationRequest>>,
        Vec<RequestTag>,
        Option<NonZeroUsize>,
    ) {
        match self {
            Self::Compile { tags, max_parallel } => (None, tags, max_parallel),
            Self::Simulate {
                requests,
                tags,
                max_parallel,
            } => (Some(requests), tags, max_parallel),
        }
    }

    /// Merges a later request into this queued one.
    ///
    /// Two compiles stay a compile; anything with a simulate becomes a simulate. Tags and
    /// requests are united, keeping the order they were first requested in; requests with the
    /// same pattern merge into one that runs paused if either does. The merged operation has
    /// the smallest process limit of the two, so that it keeps the promise of either request.
    #[must_use]
    pub(super) fn merge(self, later: Self) -> Self {
        let (requests, mut merged_tags, max_parallel) = self.into_parts();
        let (later_requests, later_tags, later_max_parallel) = later.into_parts();
        for tag in later_tags {
            if !merged_tags.contains(&tag) {
                merged_tags.push(tag);
            }
        }
        let max_parallel = min_limit(max_parallel, later_max_parallel);
        match (requests, later_requests) {
            (None, None) => Self::Compile {
                tags: merged_tags,
                max_parallel,
            },
            (requests, later_requests) => Self::Simulate {
                requests: merge_requests(
                    requests.unwrap_or_default(),
                    later_requests.unwrap_or_default(),
                ),
                tags: merged_tags,
                max_parallel,
            },
        }
    }
}

/// The smaller of two process limits; `None` doesn't limit.
fn min_limit(limit: Option<NonZeroUsize>, other: Option<NonZeroUsize>) -> Option<NonZeroUsize> {
    match (limit, other) {
        (Some(limit), Some(other)) => Some(limit.min(other)),
        (limit, None) | (None, limit) => limit,
    }
}

/// Appends `later` to `requests`, merging requests with the same pattern.
fn merge_requests(
    mut requests: Vec<SimulationRequest>,
    later: Vec<SimulationRequest>,
) -> Vec<SimulationRequest> {
    for request in later {
        match requests
            .iter_mut()
            .find(|existing| existing.pattern == request.pattern)
        {
            Some(existing) => existing.gui |= request.gui,
            None => requests.push(request),
        }
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(clippy::unnecessary_wraps, reason = "requests take optional tags")]
    fn tag(name: &str) -> Option<RequestTag> {
        Some(RequestTag(name.to_owned()))
    }

    fn tags(names: &[&str]) -> Vec<RequestTag> {
        names
            .iter()
            .map(|name| RequestTag((*name).to_owned()))
            .collect()
    }

    fn requests(entries: &[(&str, bool)]) -> Vec<SimulationRequest> {
        entries
            .iter()
            .map(|&(pattern, gui)| SimulationRequest {
                pattern: pattern.to_owned(),
                gui,
            })
            .collect()
    }

    #[test]
    fn compiles_merge_into_a_compile() {
        let merged = Operation::compile(tag("a"), None)
            .merge(Operation::compile(None, None))
            .merge(Operation::compile(tag("b"), None))
            .merge(Operation::compile(tag("a"), None));
        assert_eq!(
            merged,
            Operation::Compile {
                tags: tags(&["a", "b"]),
                max_parallel: None,
            }
        );
        assert_eq!(merged.simulation_patterns(), None);
    }

    #[test]
    fn a_simulate_absorbs_compiles() {
        let simulate = Operation::simulate(requests(&[("x.*", false)]), tag("s"), None);
        assert_eq!(
            Operation::compile(tag("c"), None).merge(simulate.clone()),
            Operation::Simulate {
                requests: requests(&[("x.*", false)]),
                tags: tags(&["c", "s"]),
                max_parallel: None,
            }
        );
        assert_eq!(
            simulate.merge(Operation::compile(tag("c"), None)),
            Operation::Simulate {
                requests: requests(&[("x.*", false)]),
                tags: tags(&["s", "c"]),
                max_parallel: None,
            }
        );
    }

    #[test]
    fn simulates_unite_requests_by_pattern() {
        let merged = Operation::simulate(
            requests(&[("a", false), ("b", true), ("a", false)]),
            None,
            None,
        )
        .merge(Operation::simulate(
            requests(&[("c", false), ("a", true), ("b", false)]),
            tag("t"),
            None,
        ));
        assert_eq!(
            merged,
            Operation::Simulate {
                requests: requests(&[("a", true), ("b", true), ("c", false)]),
                tags: tags(&["t"]),
                max_parallel: None,
            }
        );
        assert_eq!(
            merged.simulation_patterns(),
            Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
        );
    }

    #[test]
    fn merges_keep_the_smallest_limit() {
        let limit = |n| NonZeroUsize::new(n);
        let merged = Operation::compile(tag("a"), limit(4))
            .merge(Operation::compile(tag("b"), None))
            .merge(Operation::simulate(
                requests(&[("x", false)]),
                tag("c"),
                limit(2),
            ))
            .merge(Operation::compile(tag("d"), limit(3)));
        assert_eq!(merged.max_parallel(), limit(2));
        assert_eq!(
            Operation::compile(None, None)
                .merge(Operation::compile(None, None))
                .max_parallel(),
            None
        );
        assert_eq!(
            Operation::compile(None, None)
                .merge(Operation::compile(None, limit(1)))
                .max_parallel(),
            limit(1)
        );
    }
}
