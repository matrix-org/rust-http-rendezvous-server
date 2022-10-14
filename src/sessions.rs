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

use std::{
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::TypedHeader;
use base64ct::Encoding;
use bytes::Bytes;
use headers::{ContentType, ETag, Expires, LastModified};
use mime::Mime;
use sha2::Digest;
use tokio::sync::{Mutex, RwLock, RwLockMappedWriteGuard, RwLockReadGuard, RwLockWriteGuard};
use ulid::Ulid;

pub struct Session {
    hash: [u8; 32],
    data: Bytes,
    content_type: Mime,
    last_modified: SystemTime,
    expires: SystemTime,
}

impl Session {
    fn new(data: Bytes, content_type: Mime, ttl: Duration) -> Self {
        let hash = sha2::Sha256::digest(&data).into();
        let now = SystemTime::now();
        Self {
            hash,
            data,
            content_type,
            expires: now + ttl,
            last_modified: now,
        }
    }

    pub fn update(&mut self, data: Bytes, content_type: Mime) {
        self.hash = sha2::Sha256::digest(&data).into();
        self.data = data;
        self.content_type = content_type;
        self.last_modified = SystemTime::now();
    }

    pub fn content_type(&self) -> ContentType {
        self.content_type.clone().into()
    }

    pub fn etag(&self) -> ETag {
        let encoded = base64ct::Base64Url::encode_string(&self.hash);
        // SAFETY: Base64 encoding is URL-safe, so ETag-safe
        format!("\"{encoded}\"")
            .parse()
            .expect("base64-encoded hash should be URL-safe")
    }

    pub fn data(&self) -> Bytes {
        self.data.clone()
    }

    fn last_modified(&self) -> LastModified {
        self.last_modified.into()
    }

    fn expires(&self) -> Expires {
        self.expires.into()
    }

    pub fn typed_headers(
        &self,
    ) -> (
        TypedHeader<ETag>,
        TypedHeader<Expires>,
        TypedHeader<LastModified>,
    ) {
        (
            TypedHeader(self.etag()),
            TypedHeader(self.expires()),
            TypedHeader(self.last_modified()),
        )
    }
}

#[derive(Clone, Default)]
pub struct Sessions {
    inner: Arc<RwLock<BTreeMap<Ulid, Session>>>,
    generator: Arc<Mutex<ulid::Generator>>,
    capacity: usize,
    hard_capacity: usize,
    ttl: Duration,
}

/// Evict the keys at the beginning of a [`BTreeMap`], up to ``capacity``
fn evict<K: Copy + Ord, V>(sessions: &mut BTreeMap<K, V>, capacity: usize) {
    // NOTE: eviction is based on the fact that ULIDs are monotonically increasing,
    // by evictin the keys at the head of the map

    // List of keys to evict
    let keys: Vec<K> = sessions
        .keys()
        .take(sessions.len() - capacity)
        .copied()
        .collect();

    // Now evict the keys
    for key in keys {
        sessions.remove(&key);
    }
}

impl Sessions {
    /// Create a new session store with the given parameters
    #[must_use]
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
            generator: Arc::new(Mutex::new(ulid::Generator::new())),
            ttl,
            capacity,
            hard_capacity: capacity * 2,
        }
    }

    /// Create and insert a new session in the store
    pub async fn new_session(
        &self,
        payload: Bytes,
        content_type: Mime,
    ) -> (Ulid, RwLockReadGuard<Session>) {
        let id = self.generate_id().await;
        let session = Session::new(payload, content_type, self.ttl);
        let session = self.insert(id, session, self.ttl).await;
        (id, session)
    }

    /// Find a session in the store
    pub async fn get_session(&self, id: Ulid) -> Option<RwLockReadGuard<Session>> {
        let sessions = self.inner.read().await;
        RwLockReadGuard::try_map(sessions, |sessions| sessions.get(&id)).ok()
    }

    /// Get a mutable reference to a session from the store
    pub async fn get_session_mut(&self, id: Ulid) -> Option<RwLockMappedWriteGuard<Session>> {
        let sessions = self.inner.write().await;
        RwLockWriteGuard::try_map(sessions, |sessions| sessions.get_mut(&id)).ok()
    }

    /// Delete a session from the store
    pub async fn delete_session(&self, id: Ulid) -> bool {
        self.inner.write().await.remove(&id).is_some()
    }

    async fn insert(&self, id: Ulid, session: Session, ttl: Duration) -> RwLockReadGuard<Session> {
        let mut sessions = self.inner.write().await;

        // When inserting, we check if we will hit the 'hard' capacity, so that we never
        // go over that capacity
        if sessions.len() + 1 >= self.hard_capacity {
            evict(&mut sessions, self.capacity);
        }

        sessions.insert(id, session);

        // Downgrade the write lock to a read lock, to get a reference to the
        // just-inserted session
        let session = RwLockReadGuard::map(sessions.downgrade(), |sessions| {
            sessions
                .get(&id)
                .expect("Session should be in the map just after insertion")
        });

        let this = self.clone();
        // TODO: cancel this task when an item gets deleted
        tokio::task::spawn(async move {
            tokio::time::sleep(ttl).await;
            this.delete_session(id).await;
        });

        session
    }

    async fn generate_id(&self) -> Ulid {
        self.generator
            .lock()
            .await
            .generate()
            // This would panic the thread if too many IDs (more than 2^40) are generated on the
            // same millisecond, which is very unlikely
            .expect("Failed to generate random ID")
    }

    /// A loop which evicts keys if the capacity is reached
    pub fn eviction_task(
        &self,
        interval: Duration,
    ) -> impl Future<Output = ()> + Send + Sync + 'static {
        let this = self.clone();
        async move {
            let mut interval = tokio::time::interval(interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;
                this.evict().await;
            }
        }
    }

    /// Trigger an eviction, removing the oldest entries if the soft capacity
    /// has been reached
    pub(crate) async fn evict(&self) {
        if self.inner.read().await.len() > self.capacity {
            let mut sessions = self.inner.write().await;
            evict(&mut sessions, self.capacity);
        }
    }

    /// Fill the sessions storage to check how much memory it might use on max
    /// capacity
    ///
    /// # Panics
    ///
    /// It panics if the session storage is not empty
    pub async fn fill_for_mem_check(&self, entry_size: usize) {
        let mut sessions = self.inner.write().await;
        let mut generator = self.generator.lock().await;
        assert!(sessions.is_empty());

        let data: Vec<u8> = std::iter::repeat(42).take(entry_size).collect();
        sessions.extend((0..self.capacity).map(|_| {
            let data = Bytes::from(data.clone());
            let id = generator.generate().unwrap();
            let session = Session::new(data, mime::APPLICATION_OCTET_STREAM, self.ttl);
            (id, session)
        }));

        // Start the deletion tasks for all the sessions
        let ttl = self.ttl;
        for &key in sessions.keys() {
            let inner = self.inner.clone();
            tokio::task::spawn(async move {
                tokio::time::sleep(ttl).await;
                inner.write().await.remove(&key);
            });
        }
    }
}
