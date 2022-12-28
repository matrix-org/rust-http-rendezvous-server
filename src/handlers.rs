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

use axum::{
    body::HttpBody,
    extract::{DefaultBodyLimit, Path, State},
    http::{
        header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, LOCATION},
        StatusCode,
    },
    response::{IntoResponse, Response},
    routing::{get, post},
    Router, TypedHeader,
};
use bytes::Bytes;
use headers::{ContentType, HeaderName, HeaderValue, IfMatch, IfNoneMatch};
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    set_header::SetResponseHeaderLayer,
};
use ulid::Ulid;

use crate::Sessions;

async fn new_session(
    State(sessions): State<Sessions>,
    content_type: Option<TypedHeader<ContentType>>,
    payload: Bytes,
) -> impl IntoResponse {
    let content_type =
        content_type.map_or(mime::APPLICATION_OCTET_STREAM, |TypedHeader(c)| c.into());
    let (id, session) = sessions.new_session(payload, content_type).await;
    let headers = session.typed_headers();

    let location = id.to_string();
    let additional_headers = [(LOCATION, location)];
    (StatusCode::CREATED, headers, additional_headers)
}

async fn delete_session(State(sessions): State<Sessions>, Path(id): Path<Ulid>) -> StatusCode {
    if sessions.delete_session(id).await {
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
    if let Some(mut session) = sessions.get_session_mut(id).await {
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
    let session = if let Some(session) = sessions.get_session(id).await {
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
        session.data(),
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
    let router = Router::new()
        .route("/", post(new_session))
        .route(
            "/:id",
            get(get_session).put(update_session).delete(delete_session),
        )
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
        );

    Router::new().nest(prefix, router).with_state(sessions)
}
