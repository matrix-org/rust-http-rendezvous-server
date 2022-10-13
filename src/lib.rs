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
    collections::HashMap,
    ops::Deref,
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
use tokio::sync::RwLock;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    set_header::SetResponseHeaderLayer,
};
use uuid::Uuid;

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
struct Sessions {
    // TODO: is that global lock alright?
    inner: Arc<RwLock<HashMap<Uuid, Session>>>,
    ttl: Duration,
}

impl Sessions {
    async fn insert(self, id: Uuid, session: Session, ttl: Duration) {
        self.inner.write().await.insert(id, session);
        // TODO: cancel this task when an item gets deleted
        tokio::task::spawn(async move {
            tokio::time::sleep(ttl).await;
            self.inner.write().await.remove(&id);
        });
    }
}

impl Deref for Sessions {
    type Target = RwLock<HashMap<Uuid, Session>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
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
    // TODO: should we use something else? Check for colisions?
    let id = Uuid::new_v4();
    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let session = Session::new(payload, content_type, ttl);
    let headers = session.typed_headers();
    sessions.insert(id, session, ttl).await;

    let location = id.to_string();
    let additional_headers = [(LOCATION, location)];
    (StatusCode::CREATED, headers, additional_headers)
}

async fn delete_session(State(sessions): State<Sessions>, Path(id): Path<Uuid>) -> StatusCode {
    if sessions.write().await.remove(&id).is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn update_session(
    State(sessions): State<Sessions>,
    Path(id): Path<Uuid>,
    content_type: Option<TypedHeader<ContentType>>,
    if_match: Option<TypedHeader<IfMatch>>,
    payload: Bytes,
) -> Response {
    if let Some(session) = sessions.write().await.get_mut(&id) {
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
    Path(id): Path<Uuid>,
    if_none_match: Option<TypedHeader<IfNoneMatch>>,
) -> Response {
    let sessions = sessions.read().await;
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
pub fn router<B>(prefix: &str, ttl: Duration, max_bytes: usize) -> Router<(), B>
where
    B: HttpBody + Send + 'static,
    <B as HttpBody>::Data: Send,
    <B as HttpBody>::Error: std::error::Error + Send + Sync,
{
    let sessions = Sessions {
        inner: Arc::default(),
        ttl,
    };

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
        let app = router("/", ttl, 4096);

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
    async fn test_post_max_bytes() {
        let ttl = Duration::from_secs(60);

        let body = br#"{"hello": "world"}"#;

        // It doesn't work with a way too small size
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", ttl, 8).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // It works with exactly the right size
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", ttl, body.len()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // It doesn't work even if the size is one too short
        let slow_body = SlowBody::from_static(body);
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .body(slow_body)
            .unwrap();
        let response = router("/", ttl, body.len() - 1)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Try with a big body (4MB), sent in small 128 bytes chunks
        let body = vec![42; 4 * 1024 * 1024].into_boxed_slice();
        let slow_body = SlowBody::from_bytes(Bytes::from(body)).with_chunk_size(128);
        let request = Request::post("/").body(slow_body).unwrap();
        let response = router("/", ttl, 4 * 1024 * 1024)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Try with a big body (4MB + 1B), sent in small 128 bytes chunks
        let body = vec![42; 4 * 1024 * 1024 + 1].into_boxed_slice();
        let slow_body = SlowBody::from_bytes(Bytes::from(body)).with_chunk_size(128);
        let request = Request::post("/").body(slow_body).unwrap();
        let response = router("/", ttl, 4 * 1024 * 1024)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn test_post_and_get_if_none_match() {
        let ttl = Duration::from_secs(60);
        let app = router("/", ttl, 4096);

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
        let app = router("/", ttl, 4096);

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
        let app = router("/", ttl, 4096);

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
        let app = router("/", ttl, 4096);

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
}
