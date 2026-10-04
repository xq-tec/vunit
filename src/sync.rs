// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Synchronization helpers.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

/// Locks `mutex`, ignoring that a holder panicked. Callers state why the data stays consistent.
pub(crate) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
