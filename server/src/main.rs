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

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use bytesize::ByteSize;
use clap::Parser;
use matrix_http_rendezvous::{DEFAULT_MAX_BYTES_STR, DEFAULT_MAX_ENTRIES, DEFAULT_TTL};

#[derive(Parser)]
#[command(version, about)]
struct Options {
    /// Address on which to listen
    #[arg(short, long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST), env = "LISTEN_ADDRESS")]
    address: IpAddr,

    /// Port on which to listen
    #[arg(short, long, default_value_t = 8090, env = "LISTEN_PORT")]
    port: u16,

    /// Path prefix on which to mount the rendez-vous server
    #[arg(long)]
    prefix: Option<String>,

    /// Time to live of entries, in seconds
    #[arg(short, long, default_value_t = DEFAULT_TTL.into())]
    ttl: humantime::Duration,

    /// Maximum number of entries to store
    #[arg(short, long, default_value_t = DEFAULT_MAX_ENTRIES)]
    capacity: usize,

    /// Maximum payload size, in bytes
    #[arg(short, long, default_value = DEFAULT_MAX_BYTES_STR)]
    max_bytes: ByteSize,

    /// Set this flag to test how much memory the server might use with a
    /// sessions map fully loaded
    #[arg(long)]
    mem_check: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let options = Options::parse();
    let prefix = options.prefix.unwrap_or_default();
    let ttl = options.ttl.into();
    let max_bytes = options
        .max_bytes
        .0
        .try_into()
        .expect("Max bytes size too large");

    let sessions = matrix_http_rendezvous::Sessions::new(ttl, options.capacity);

    let signal = async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install signal handler");
        tracing::info!("SIGINT received, shutting down");
    };

    if options.mem_check {
        tracing::info!(
            "Filling cache with {capacity} entries of {max_bytes}",
            capacity = options.capacity,
            max_bytes = options.max_bytes.to_string_as(true)
        );
        sessions.fill_for_mem_check(max_bytes).await;
        tracing::info!("Done filling, waiting 60 seconds");

        let sleep = tokio::time::sleep(Duration::from_secs(60));
        tokio::select! {
            _ = signal => {},
            _ = sleep => {},
        };

        return;
    }

    tokio::spawn(sessions.eviction_task(Duration::from_secs(60)));

    let addr = SocketAddr::from((options.address, options.port));

    let service = matrix_http_rendezvous::router(&prefix, sessions, max_bytes);

    tracing::info!("Listening on http://{addr}");
    tracing::info!(
        "TTL: {ttl} – Maximum payload size: {max_bytes}",
        ttl = humantime::format_duration(ttl),
        max_bytes = options.max_bytes.to_string_as(true)
    );

    hyper::Server::bind(&addr)
        .serve(service.into_make_service())
        .with_graceful_shutdown(signal)
        .await
        .unwrap();
}
