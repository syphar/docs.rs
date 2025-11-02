# just commands for CI & local development

sqlx-prepare *args:
  cargo sqlx prepare \
    --database-url $DOCSRS_DATABASE_URL \
    --workspace {{ args }} \
    -- --all-targets --all-features

sqlx-check:
  just sqlx-prepare -check

lint: 
  cargo clippy --all-features --all-targets --workspace --locked -- -D warnings

lint-js *args:
  deno run -A npm:eslint@9 static templates gui-tests eslint.config.js {{ args }}
  
gui-tests:
  #!/usr/bin/env bash

  set -e

  # FIXME: @syphar, it might be easy to update the script with the new 
  # docker compose setup to make it run on non-linux machines. 
  # Perhasp we could even replace the script just with some "Justfile"
  # tasks.

  # Just in case it's running, we stop the web server.
  docker compose stop web

  docker compose up --wait --wait-timeout 30 db s3

  # If we have a .env file, we need to temporarily move it so
  # it doesn't make sqlx fail compilation.
  if [ -f .env ]; then
    mv .env .tmp.env
  fi

  # We add the information we need.
  cargo run -- database migrate
  cargo run -- build update-toolchain
  cargo run -- build crate sysinfo 0.23.4
  cargo run -- build crate sysinfo 0.23.5
  cargo run -- build crate libtest 0.0.1
  cargo run -- build add-essential-files

  if [ -f .tmp.env ]; then
    mv .tmp.env .env
  fi

  # In case we don't have a `.env`, we create one.
  if [ ! -f .env ]; then
    cp .env.sample .env
  fi

  . .env

  set +e # We disable the "exit right away if command failed" setting.
  cargo run -- start-web-server &
  SERVER_PID=$!

  # status="docker run . -v `pwd`:/build/out:ro gui_tests"
  docker compose build gui_tests
  docker compose run --rm --remove-orphans gui_tests
  status=$?
  kill $SERVER_PID
  exit $status
