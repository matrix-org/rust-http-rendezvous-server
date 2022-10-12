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

use clap::Parser;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

#[derive(Parser)]
struct Options {
    /// Address on which to listen
    #[arg(short, long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    address: IpAddr,

    /// Port on which to listen
    #[arg(short, long, default_value_t = 8090)]
    port: u16,

    /// Path prefix on which to mount the rendez-vous server
    #[arg(long)]
    prefix: Option<String>,

    /// Time to live of entries, in seconds
    #[arg(short, long, default_value_t = 60)]
    ttl: u64,

    /// Maximum payload size, in bytes
    #[arg(short, long, default_value_t = 4096)]
    max_bytes: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let options = Options::parse();
    let prefix = options.prefix.unwrap_or_default();
    let ttl = Duration::from_secs(options.ttl);

    let addr = SocketAddr::from((options.address, options.port));

    let service = matrix_http_rendezvous::router(&prefix, ttl, options.max_bytes);

    tracing::info!("Listening on http://{addr}");
    tracing::info!(
        "TTL: {ttl}s – Maximum payload size: {max_bytes} bytes",
        ttl = ttl.as_secs(),
        max_bytes = options.max_bytes
    );

    hyper::Server::bind(&addr)
        .serve(service.into_make_service())
        .await
        .unwrap();
}
