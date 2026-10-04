// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Helpers shared by the test binaries.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::future::Future;

use libtest_mimic::Trial;

/// A trial that runs the future of `test` on its own multi-threaded runtime.
pub fn trial<F: Future<Output = ()>>(
    name: &str,
    test: impl FnOnce() -> F + Send + 'static,
) -> Trial {
    Trial::test(name, move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build the runtime")
            .block_on(test());
        Ok(())
    })
}
