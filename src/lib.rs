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

use std::{collections::HashMap, sync::Arc, time::SystemTime};

use axum::{
    body::HttpBody,
    extract::{ContentLengthLimit, Path},
    http::{header::LOCATION, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Router, TypedHeader,
};
use base64ct::Encoding;
use bytes::Bytes;
use headers::{ContentType, ETag, HeaderName, IfNoneMatch, LastModified};
use mime::Mime;
use sha2::Digest;
use tokio::sync::RwLock;
use uuid::Uuid;

// TODO: config?
const MAX_BYTES: u64 = 4096;

// TODO: expire the sessions
struct Session {
    hash: [u8; 32],
    data: Bytes,
    content_type: Mime,
    last_modified: SystemTime,
}

impl Session {
    fn new(data: Bytes, content_type: Mime) -> Self {
        let hash = sha2::Sha256::digest(&data).into();
        Self {
            hash,
            data,
            content_type,
            last_modified: SystemTime::now(),
        }
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
}

struct State {
    // TODO: is that global lock alright?
    sessions: RwLock<HashMap<Uuid, Session>>,
    prefix: String,
}

impl State {
    fn new(prefix: String) -> Self {
        Self {
            sessions: RwLock::default(),
            prefix,
        }
    }
}

async fn new_session(
    Extension(state): Extension<Arc<State>>,
    content_type: Option<TypedHeader<ContentType>>,
    // TODO: this requires a Content-Length header, is that alright?
    ContentLengthLimit(payload): ContentLengthLimit<Bytes, MAX_BYTES>,
) -> impl IntoResponse {
    // TODO: should we use something else? Check for colisions?
    let id = Uuid::new_v4();
    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let session = Session::new(payload, content_type);
    state.sessions.write().await.insert(id, session);

    // TODO: actually join with the prefix, to prevent accidental double slashes
    let location = format!("{}/{}", state.prefix, id);
    let headers = [
        (LOCATION, location),
        (
            HeaderName::from_static("x-max-bytes"),
            MAX_BYTES.to_string(),
        ),
    ];
    (StatusCode::NO_CONTENT, headers)
}
async fn update_session(
    Extension(state): Extension<Arc<State>>,
    Path(id): Path<Uuid>,
    content_type: Option<TypedHeader<ContentType>>,
    ContentLengthLimit(payload): ContentLengthLimit<Bytes, MAX_BYTES>,
) -> StatusCode {
    if state.sessions.read().await.get(&id).is_none() {
        return StatusCode::NOT_FOUND;
    }

    // TODO: check the if-match header
    // TODO: probably missing some response headers

    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let session = Session::new(payload, content_type);
    state.sessions.write().await.insert(id, session);
    StatusCode::ACCEPTED
}

async fn get_session(
    Extension(state): Extension<Arc<State>>,
    Path(id): Path<Uuid>,
    if_none_match: Option<TypedHeader<IfNoneMatch>>,
) -> Response {
    let sessions = state.sessions.read().await;
    let session = if let Some(session) = sessions.get(&id) {
        session
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let etag = session.etag();
    if let Some(TypedHeader(if_none_match)) = if_none_match {
        if if_none_match.precondition_passes(&etag) {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }

    (
        StatusCode::OK,
        TypedHeader(session.content_type()),
        TypedHeader(session.last_modified()),
        TypedHeader(etag),
        session.data.clone(),
    )
        .into_response()
}

#[must_use]
pub fn router<B>(prefix: &str) -> Router<B>
where
    B: HttpBody + Send + 'static,
    <B as HttpBody>::Data: Send,
    <B as HttpBody>::Error: std::error::Error + Send + Sync,
{
    // TODO: switch to axum 0.6 state
    let state = Arc::new(State::new(prefix.to_owned()));
    let router = Router::new()
        .route("/", post(new_session))
        .route("/:id", get(get_session).put(update_session))
        .layer(Extension(state));

    Router::new().nest(prefix, router)
}
