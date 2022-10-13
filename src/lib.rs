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
#![allow(clippy::trait_duplication_in_bounds)]

use std::{
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    body::HttpBody,
    extract::{DefaultBodyLimit, FromRef, Path, State},
    http::{
        header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, LOCATION},
        StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Router, TypedHeader,
};
use base64ct::Encoding;
use bytes::Bytes;
use headers::{
    ContentType, ETag, Expires, HeaderName, HeaderValue, IfMatch, IfNoneMatch, LastModified,
};
use mime::Mime;
use sha2::Digest;
use tokio::sync::{Mutex, RwLock};
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    set_header::SetResponseHeaderLayer,
};
use ulid::Ulid;

struct Session {
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

    fn update(&mut self, data: Bytes, content_type: Mime) {
        self.hash = sha2::Sha256::digest(&data).into();
        self.data = data;
        self.content_type = content_type;
        self.last_modified = SystemTime::now();
    }

    fn content_type(&self) -> ContentType {
        self.content_type.clone().into()
    }

    fn etag(&self) -> ETag {
        let encoded = base64ct::Base64Url::encode_string(&self.hash);
        // SAFETY: Base64 encoding is URL-safe, so ETag-safe
        format!("\"{encoded}\"").parse().unwrap()
    }

    fn last_modified(&self) -> LastModified {
        self.last_modified.into()
    }

    fn expires(&self) -> Expires {
        self.expires.into()
    }

    fn typed_headers(
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

fn evict(sessions: &mut BTreeMap<Ulid, Session>, capacity: usize) {
    // NOTE: eviction is based on the fact that ULIDs are monotonically increasing, by evictin the
    // keys at the head of the map

    // List of keys to evict
    let keys: Vec<Ulid> = sessions
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

    async fn insert(self, id: Ulid, session: Session, ttl: Duration) {
        {
            let mut sessions = self.inner.write().await;
            sessions.insert(id, session);
            // When inserting, we check if we hit the 'hard' capacity, so that we never go over
            // that capacity
            if sessions.len() >= self.hard_capacity {
                evict(&mut sessions, self.capacity);
            }
        }

        // TODO: cancel this task when an item gets deleted
        tokio::task::spawn(async move {
            tokio::time::sleep(ttl).await;
            self.inner.write().await.remove(&id);
        });
    }

    async fn generate_id(&self) -> Ulid {
        self.generator
            .lock()
            .await
            .generate()
            // This would panic the thread if too many IDs (more than 2^40) are generated on the same
            // millisecond, which is very unlikely
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

    async fn evict(&self) {
        if self.inner.read().await.len() > self.capacity {
            let mut sessions = self.inner.write().await;
            evict(&mut sessions, self.capacity);
        }
    }

    /// Fill the sessions storage to check how much memory it might use on max capacity
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

impl FromRef<AppState> for Sessions {
    fn from_ref(input: &AppState) -> Self {
        input.sessions.clone()
    }
}

struct AppState {
    sessions: Sessions,
}

impl AppState {
    fn new(sessions: Sessions) -> Self {
        Self { sessions }
    }
}

async fn new_session(
    State(sessions): State<Sessions>,
    content_type: Option<TypedHeader<ContentType>>,
    payload: Bytes,
) -> impl IntoResponse {
    let ttl = sessions.ttl;

    let id = sessions.generate_id().await;

    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let session = Session::new(payload, content_type, ttl);
    let headers = session.typed_headers();
    sessions.insert(id, session, ttl).await;

    let location = id.to_string();
    let additional_headers = [(LOCATION, location)];
    (StatusCode::CREATED, headers, additional_headers)
}

async fn delete_session(State(sessions): State<Sessions>, Path(id): Path<Ulid>) -> StatusCode {
    if sessions.inner.write().await.remove(&id).is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn update_session(
    State(sessions): State<Sessions>,
    Path(id): Path<Ulid>,
    content_type: Option<TypedHeader<ContentType>>,
    if_match: Option<TypedHeader<IfMatch>>,
    payload: Bytes,
) -> Response {
    if let Some(session) = sessions.inner.write().await.get_mut(&id) {
        if let Some(TypedHeader(if_match)) = if_match {
            if !if_match.precondition_passes(&session.etag()) {
                return (StatusCode::PRECONDITION_FAILED, session.typed_headers()).into_response();
            }
        }

        let content_type =
            content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());

        session.update(payload, content_type);
        (StatusCode::ACCEPTED, session.typed_headers()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn get_session(
    State(sessions): State<Sessions>,
    Path(id): Path<Ulid>,
    if_none_match: Option<TypedHeader<IfNoneMatch>>,
) -> Response {
    let sessions = sessions.inner.read().await;
    let session = if let Some(session) = sessions.get(&id) {
        session
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(TypedHeader(if_none_match)) = if_none_match {
        if !if_none_match.precondition_passes(&session.etag()) {
            return (StatusCode::NOT_MODIFIED, session.typed_headers()).into_response();
        }
    }

    (
        StatusCode::OK,
        session.typed_headers(),
        TypedHeader(session.content_type()),
        session.data.clone(),
    )
        .into_response()
}

#[must_use]
pub fn router<B>(prefix: &str, sessions: Sessions, max_bytes: usize) -> Router<(), B>
where
    B: HttpBody + Send + 'static,
    <B as HttpBody>::Data: Send,
    <B as HttpBody>::Error: std::error::Error + Send + Sync,
{
    let state = AppState::new(sessions);
    let router = Router::with_state(state)
        .route("/", post(new_session))
        .route(
            "/:id",
            get(get_session).put(update_session).delete(delete_session),
        );

    Router::new()
        .nest(prefix, router)
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(max_bytes))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-max-bytes"),
            HeaderValue::from_str(&max_bytes.to_string())
                .expect("Could not construct x-max-bytes header value"),
        ))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers([CONTENT_TYPE, IF_MATCH, IF_NONE_MATCH])
                .expose_headers([ETAG, LOCATION, HeaderName::from_static("x-max-bytes")]),
        )
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    use axum::http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        Request,
    };
    use bytes::Buf;
    use tower::util::ServiceExt;

    /// A slow body, which sends the bytes in small chunks (1 byte per chunk by default)
    #[derive(Clone)]
    struct SlowBody {
        body: Bytes,
        chunk_size: usize,
    }

    impl SlowBody {
        const fn from_static(bytes: &'static [u8]) -> Self {
            Self {
                body: Bytes::from_static(bytes),
                chunk_size: 1,
            }
        }

        const fn from_bytes(body: Bytes) -> Self {
            Self {
                body,
                chunk_size: 1,
            }
        }

        const fn with_chunk_size(mut self, chunk_size: usize) -> Self {
            self.chunk_size = chunk_size;
            self
        }
    }

    impl HttpBody for SlowBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_data(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<Self::Data, Self::Error>>> {
            if self.body.is_empty() {
                std::task::Poll::Ready(None)
            } else {
                let size = self.chunk_size.min(self.body.len());
                let ret = self.body.slice(0..size);
                self.get_mut().body.advance(size);
                std::task::Poll::Ready(Some(Ok(ret)))
            }
        }

        fn poll_trailers(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<Option<headers::HeaderMap>, Self::Error>> {
            std::task::Poll::Ready(Ok(None))
        }
    }

    async fn advance_time(duration: Duration) {
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn test_post_and_get() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions, 4096);

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();
        let etag = response.headers().get(ETAG).unwrap();
        let url = format!("/{location}");

        let request = Request::get(&url).body(String::new()).unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(response.headers().get(ETAG).unwrap(), etag);

        // Let the entry expire
        advance_time(ttl + Duration::from_secs(1)).await;

        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(&body[..], br#"{"hello": "world"}"#);

        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_monotonically_increasing() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions.clone(), 4096);

        // Prepare a thousand requests
        let mut requests = Vec::with_capacity(1000);
        for _ in 0..requests.capacity() {
            requests.push(
                app.clone()
                    .oneshot(Request::post("/").body(String::new()).unwrap()),
            );
        }

        // Run them all in order
        let mut responses = Vec::with_capacity(requests.len());
        for fut in requests {
            responses.push(fut.await);
        }

        // Get the location out of them
        let ids: Vec<_> = responses
            .iter()
            .map(|res| {
                let res = res.as_ref().unwrap();
                assert_eq!(res.status(), StatusCode::CREATED);
                res.headers().get(LOCATION).unwrap().to_str().unwrap()
            })
            .collect();

        // Check that all the IDs are monotonically increasing
        assert!(ids.windows(2).all(|loc| loc[0] < loc[1]));
    }

    #[tokio::test]
    async fn test_post_max_bytes() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);

        let body = br#"{"hello": "world"}"#;

        // It doesn't work with a way too small size
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", sessions.clone(), 8)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // It works with exactly the right size
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", sessions.clone(), body.len())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // It doesn't work even if the size is one too short
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", sessions.clone(), body.len() - 1)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Try with a big body (4MB), sent in small 128 bytes chunks
        let body = vec![42; 4 * 1024 * 1024].into_boxed_slice();
        let slow_body = SlowBody::from_bytes(Bytes::from(body)).with_chunk_size(128);
        let request = Request::post("/").body(slow_body).unwrap();
        let response = router("/", sessions.clone(), 4 * 1024 * 1024)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Try with a big body (4MB + 1B), sent in small 128 bytes chunks
        let body = vec![42; 4 * 1024 * 1024 + 1].into_boxed_slice();
        let slow_body = SlowBody::from_bytes(Bytes::from(body)).with_chunk_size(128);
        let request = Request::post("/").body(slow_body).unwrap();
        let response = router("/", sessions.clone(), 4 * 1024 * 1024)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn test_post_and_get_if_none_match() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions, 4096);

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();
        let etag = response.headers().get(ETAG).unwrap();
        let url = format!("/{location}");

        let request = Request::get(&url)
            .header("if-none-match", etag)
            .body(String::new())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers().get(ETAG).unwrap(), etag);
    }

    #[tokio::test]
    async fn test_post_and_put() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions, 4096);

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();
        let etag = response.headers().get(ETAG).unwrap().to_str().unwrap();
        let url = format!("/{location}");

        let request = Request::put(&url)
            .header(CONTENT_LENGTH, 0)
            .body(String::new())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_ne!(response.headers().get(ETAG).unwrap(), etag);
    }

    #[tokio::test]
    async fn test_post_and_put_if_match() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions, 4096);

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();
        let etag = response.headers().get(ETAG).unwrap().to_str().unwrap();
        let url = format!("/{location}");

        let request = Request::put(&url)
            .header("if-match", etag)
            .header(CONTENT_LENGTH, 0)
            .body(String::new())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let etag2 = response.headers().get(ETAG).unwrap().to_str().unwrap();
        assert_ne!(etag2, etag);

        let request = Request::put(&url)
            .header("if-match", etag)
            .header(CONTENT_LENGTH, 0)
            .body(String::new())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
        assert_eq!(response.headers().get(ETAG).unwrap(), etag2);
    }

    #[tokio::test]
    async fn test_post_delete_and_get() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 1024);
        let app = router("/", sessions, 4096);

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();
        let url = format!("/{location}");

        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = Request::delete(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_eviction() {
        let ttl = Duration::from_secs(60);
        let sessions = Sessions::new(ttl, 2);
        let app = router("/", sessions.clone(), 4096);

        let request = Request::post("/").body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let first_location = response.headers().get(LOCATION).unwrap().to_str().unwrap();

        let request = Request::post("/").body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let second_location = response.headers().get(LOCATION).unwrap().to_str().unwrap();

        sessions.evict().await;

        // Both entries are still there
        let url = format!("/{first_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let url = format!("/{second_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Sending a third request
        let request = Request::post("/").body(String::new()).unwrap();
        app.clone().oneshot(request).await.unwrap();

        // First entry should still be there, there was no eviction yet because we didn't hit hard
        // capacity
        let url = format!("/{first_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        sessions.evict().await;

        // First entry should be gone because of the eviction
        let url = format!("/{first_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Second entry should still be there
        let url = format!("/{second_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Sending two other requests, so we hit hard capacity
        let request = Request::post("/").body(String::new()).unwrap();
        app.clone().oneshot(request).await.unwrap();
        let request = Request::post("/").body(String::new()).unwrap();
        app.clone().oneshot(request).await.unwrap();

        // Second entry should be gone, because we hit hard capacity, even though we didn't had the
        // eviction triggered
        let url = format!("/{second_location}");
        let request = Request::get(&url).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
