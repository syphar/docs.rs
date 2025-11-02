# just commands for CI & local development

sqlx-prepare *args:
  cargo sqlx prepare \
    --database-url $DOCSRS_DATABASE_URL \
    --workspace {{ args }} \
    -- --all-targets --all-features

sqlx-check:
  just sqlx-prepare -check

# Format the code using `cargo fmt`. 
format:
    rustup component add rustfmt
    # like this we get both the non-zero exit code, and the local code is 
    # formatted.
    cargo fmt --all -- --check || { cargo fmt --all && exit 1; }

clippy *args:
  rustup component add clippy
  cargo clippy \
    --all-features \
    --all-targets \
    --workspace \
    --locked \
    {{ args}} \
    -- -D warnings 

clippy-fix:
    just clippy --fix --allow-dirty --allow-staged

# run all linters, for local development & CI
lint: format 
  #!/usr/bin/env bash
  if [ "$GITHUB_ACTIONS" = "true" ]; then
    just clippy
  else
    just clippy_fix
  fi


lint-js *args:
  deno run -A npm:eslint@9 static templates gui-tests eslint.config.js {{ args }}
  
run-gui-tests: ensure_db_and_s3_are_running cli-db-migrate compose-up-web
  just cli-build-update-toolchain
  just cli-build-crate sysinfo 0.23.4
  just cli-build-crate sysinfo 0.23.5
  just cli-build-crate libtest 0.0.1
  just cli-build-add-essential-files

  just _cli gui_tests 

# build binaries for all tests
build-tests:
  cargo test --no-run --workspace --locked


run-tests: ensure_db_and_s3_are_running build-tests
  cargo test --workspace --locked --no-fail-fast

run-builder-tests: ensure_db_and_s3_are_running build-tests
  #!/bin/bash
  set -euo pipefail

  export DOCSRS_INCLUDE_DEFAULT_TARGETS=true

  cargo test --locked -- --ignored --test-threads=1
