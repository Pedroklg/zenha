//! Test doubles for the media backend and token client, used by tests and by `zenha-e2e`.

mod backend;
mod token;
mod token_client;

use std::sync::{Mutex, MutexGuard, PoisonError};

pub use backend::{BackendCall, FakeBackend, FakeThumbnail};
pub use token::{AdminTokenCall, FakeTokenClient};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
