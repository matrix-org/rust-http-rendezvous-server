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
    borrow::Cow,
    collections::HashMap,
    ops::Deref,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    body::HttpBody,
    extract::{ContentLengthLimit, FromRef, Path, State},
    http::{header::LOCATION, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router, TypedHeader,
};
use base64ct::Encoding;
use bytes::Bytes;
use headers::{ContentType, ETag, Expires, HeaderName, IfNoneMatch, LastModified};
use mime::Mime;
use sha2::Digest;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

// TODO: config?
const MAX_BYTES: u64 = 4096;
const TTL: Duration = Duration::from_secs(60);

struct Session {
    hash: [u8; 32],
    data: Bytes,
    content_type: Mime,
    last_modified: SystemTime,
    expires: SystemTime,
}

impl Session {
    fn new(data: Bytes, content_type: Mime) -> Self {
        let hash = sha2::Sha256::digest(&data).into();
        let now = SystemTime::now();
        Self {
            hash,
            data,
            content_type,
            expires: now + TTL,
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

#[derive(Clone)]
struct Prefix {
    inner: Cow<'static, str>,
}

impl Prefix {
    fn new(prefix: impl Into<Cow<'static, str>>) -> Self {
        Self {
            inner: prefix.into(),
        }
    }

    fn location(&self, id: &Uuid) -> String {
        format!("{}/{}", self.inner, id)
    }
}

impl FromRef<AppState> for Sessions {
    fn from_ref(input: &AppState) -> Self {
        input.sessions.clone()
    }
}

impl FromRef<AppState> for Prefix {
    fn from_ref(input: &AppState) -> Self {
        input.prefix.clone()
    }
}

struct AppState {
    sessions: Sessions,
    prefix: Prefix,
}

impl AppState {
    fn new(prefix: impl Into<Cow<'static, str>>, sessions: Sessions) -> Self {
        Self {
            sessions,
            prefix: Prefix::new(prefix),
        }
    }
}

async fn new_session(
    State(sessions): State<Sessions>,
    State(prefix): State<Prefix>,
    content_type: Option<TypedHeader<ContentType>>,
    // TODO: this requires a Content-Length header, is that alright?
    ContentLengthLimit(payload): ContentLengthLimit<Bytes, MAX_BYTES>,
) -> impl IntoResponse {
    // TODO: should we use something else? Check for colisions?
    let id = Uuid::new_v4();
    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let session = Session::new(payload, content_type);
    let headers = session.typed_headers();
    sessions.insert(id, session, TTL).await;

    // TODO: actually join with the prefix, to prevent accidental double slashes
    let location = prefix.location(&id);
    let additional_headers = [
        (LOCATION, location),
        (
            HeaderName::from_static("x-max-bytes"),
            MAX_BYTES.to_string(),
        ),
    ];
    (StatusCode::NO_CONTENT, headers, additional_headers)
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
    ContentLengthLimit(payload): ContentLengthLimit<Bytes, MAX_BYTES>,
) -> Response {
    // TODO: check the if-match header
    // TODO: probably missing some response headers

    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    if let Some(session) = sessions.write().await.get_mut(&id) {
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
        if if_none_match.precondition_passes(&session.etag()) {
            return StatusCode::NOT_MODIFIED.into_response();
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
pub fn router<B>(prefix: impl Into<Cow<'static, str>>) -> Router<(), B>
where
    B: HttpBody + Send + 'static,
    <B as HttpBody>::Data: Send,
    <B as HttpBody>::Error: std::error::Error + Send + Sync,
{
    let prefix = prefix.into();
    let sessions = Sessions::default();

    let state = AppState::new(prefix.clone(), sessions);
    let router = Router::with_state(state)
        .route("/", post(new_session))
        .route(
            "/:id",
            get(get_session).put(update_session).delete(delete_session),
        )
        .layer(CorsLayer::permissive());

    Router::new().nest(&prefix, router)
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        Request,
    };
    use tower::util::ServiceExt;

    async fn advance_time(duration: Duration) {
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(duration).await;
        tokio::time::resume();
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn test_post_and_get() {
        let app = router("/");

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();

        let request = Request::get(location).body(String::new()).unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );

        // Let the entry expire
        advance_time(TTL + Duration::from_secs(1)).await;

        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        assert_eq!(&body[..], br#"{"hello": "world"}"#);

        let request = Request::get(location).body(String::new()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_post_delete_and_get() {
        let app = router("/");

        let body = r#"{"hello": "world"}"#.to_string();
        let request = Request::post("/")
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, body.len())
            .body(body)
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let location = response.headers().get(LOCATION).unwrap().to_str().unwrap();

        let request = Request::get(location).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = Request::delete(location).body(String::new()).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let request = Request::get(location).body(String::new()).unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}