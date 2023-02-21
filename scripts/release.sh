#!/bin/sh

# Use it like that: 
#
#   sh ./release.sh patch

run () {
  echo "+ $*" >&2
  # shellcheck disable=SC2048
  $*
}

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <patch|minor|major>" >&2
  exit 1
fi

set -eu

BUMP="$1"

run git checkout main
run git pull
run cargo set-version --workspace --bump "${BUMP}"

run cd synapse/
run poetry version "${BUMP}"
VERSION="v$(poetry version -s)"
run cd ..

run git add Cargo.toml ./*/Cargo.toml synapse/pyproject.toml
run git status

printf "About to commit and push version %s. Are you sure? " "${VERSION}"
read -r REPLY

case "${REPLY}" in
  y|Y|yes|Yes|YES) 
    run git commit -a -m "${VERSION}"
    run git tag -m "${VERSION}" "${VERSION}"
    run git push
    run git push --tags
    ;;
  *) echo "Aborting." ;;
esac
