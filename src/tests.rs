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

use std::{convert::Infallible, time::Duration};

use axum::{
    body::HttpBody,
    http::{
        header::{CONTENT_LENGTH, CONTENT_TYPE},
        Request, StatusCode,
    },
};
use bytes::{Buf, Bytes};
use hyper::header::{ETAG, LOCATION};
use tower::util::ServiceExt;

use crate::{router, Sessions};

/// A slow body, which sends the bytes in small chunks
/// (1 byte per chunk by default)
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

    // First entry should still be there, there was no eviction yet because we
    // didn't hit hard capacity
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

    // Second entry should be gone, because we hit hard capacity, even though we
    // didn't had the eviction triggered
    let url = format!("/{second_location}");
    let request = Request::get(&url).body(String::new()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
