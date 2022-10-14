// Copyright 2022 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![forbid(unsafe_code)]
#![deny(clippy::all)]
#![warn(clippy::pedantic)]

use std::time::Duration;

mod handlers;
mod sessions;
#[cfg(test)]
mod tests;

/// Default TTL of entries
pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

/// Default max number of entries
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// Default max size of each entry
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024;

#[doc(hidden)]
pub const DEFAULT_MAX_BYTES_STR: &str = "4KiB";

pub use self::{
    handlers::router,
    sessions::{Session, Sessions},
};
