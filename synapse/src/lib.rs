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
#![allow(clippy::needless_pass_by_value)]

use anyhow::anyhow;
use http_body::Body;
use pyo3::prelude::*;
use serde::Deserialize;
use tower::ServiceExt;

use pyo3_matrix_synapse_module::{parse_config, ModuleApi};

#[pyclass]
#[derive(Deserialize)]
struct Config {
    prefix: String,
}

#[pyclass]
pub struct SynapseRendezvousModule;

#[pymethods]
impl SynapseRendezvousModule {
    #[new]
    fn new(config: &Config, module_api: ModuleApi) -> PyResult<Self> {
        let service = matrix_http_rendezvous::router(&config.prefix)
            .map_response(|res| res.map(|b| b.map_err(|e| anyhow!(e))));

        module_api.register_web_service(&config.prefix, service)?;
        Ok(Self)
    }

    #[staticmethod]
    fn parse_config(config: &PyAny) -> PyResult<Config> {
        parse_config(config)
    }
}

#[pymodule]
fn matrix_http_rendezvous_synapse(_py: Python, m: &PyModule) -> PyResult<()> {
    pyo3_log::init();

    m.add_class::<SynapseRendezvousModule>()?;
    Ok(())
}
