# Rust HTTP Rendezvous Server

## This repository is archived

This repository implemented [MSC3886](https://github.com/matrix-org/matrix-spec-proposals/pull/3886) which has been closed and superseded by [MSC4108](https://github.com/matrix-org/matrix-spec-proposals/pull/4108). As such, this repository is now archived.

---
An implementation of [MSC3886: Simple rendezvous capability](https://github.com/matrix-org/matrix-spec-proposals/pull/3886) that can be used standalone or packaged as a Synapse module.

Functionality constraints:

- the in progress rendezvous do not need to be persisted between server restarts
- the server does not need to work in a clustered/sharded deployment
- no authentication is needed for use of the server

## Installation

For instructions on usage as a Synapse module, see [matrix-http-rendezvous-synapse](https://pypi.org/project/matrix-http-rendezvous-synapse/) on PyPI.

## Contributing

### Releasing

```sh
./scripts/release.sh patch
```
