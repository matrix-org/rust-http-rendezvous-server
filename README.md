# Rust HTTP Rendezvous Server

Like https://github.com/matrix-org/node-http-rendezvous-server but in Rust

An implementation of [MSC3886: Simple rendezvous capability](https://github.com/matrix-org/matrix-spec-proposals/pull/3886) that can be used standalone or packaged as a Synapse module.

Functionality constraints:

- the in progress rendezvous do not need to be persisted between server restarts
- the server does not need to work in a clustered/sharded deployment
- no authentication is needed for use of the server

### Releasing

```sh
git checkout main
cargo set-version --workspace --bump patch
cd synapse/
poetry version patch
cd ..
git commit -a -m "vX.Y.Z"
git tag vX.Y.Z
git push
git push --tags
