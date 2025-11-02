set shell := ["bash", "-euo", "pipefail", "-c"]


# List available commands
_default:
    just --list


sqlx-prepare ADDITIONAL_ARGS="":
  cargo sqlx prepare \
    --database-url $DOCSRS_DATABASE_URL \
    --workspace {{ ADDITIONAL_ARGS }} \
    -- --all-targets --all-features

sqlx-check:
  just sqlx-prepare "--check"

lint: 
  cargo clippy --all-features --all-targets --workspace --locked -- -D warnings

lint-js *args:
  deno run -A npm:eslint@9 static templates gui-tests eslint.config.js {{ args }}

_touch-docker-env:
  touch .docker.env

_compose-cli service_name *args: _touch-docker-env
  # dependencies in the docker-compose file are ignored 
  # here. Instead we explicitly start any dependent services first.
  docker compose up -d db s3 --wait
  # run the CLI command
  docker compose run --build --rm {{ service_name }} {{ args }}

# run any CLI command in its own one-off `cli` docker container. Args are passed to the container.
[group('compose')]
compose-cli *args: _touch-docker-env  compose-cli-migrate
  just _compose-cli cli {{ args }}

# Initialize the docker compose database
[group('compose')]
compose-cli-migrate: 
  # intentially not using `compose-cli`, otherweise we have a cycle.
  just _compose-cli cli database migrate

# add a release to the build queue
[group('compose')]
compose-cli-queue-add *args:
  just compose-cli queue add {{ args }}

# run builder CLI command in its own one-off docker container.
[group('compose')]
compose-cli-builder *args: _touch-docker-env compose-cli-migrate
  just _compose-cli cli-builder {{ args }}

# set the nightly rust version to be used for builds. Format: `nightly-YYYY-MM-DD`
[group('compose')]
compose-cli-set-toolchain NAME:
  just compose-cli-builder build set-toolchain {{ NAME }}

# update the toolchain in the builders
[group('compose')]
compose-cli-update-toolchain:
  just compose-cli-builder build update-toolchain

# build & the toolchain shared essential files
[group('compose')]
compose-cli-add-essential-files:
  just compose-cli-builder build add-essential-files

# build & the toolchain shared essential files
[group('compose')]
compose-cli-build-crate *args:
  just compose-cli-builder build crate {{ args }}

# run registry-watcher CLI command in its own one-off docker container.
[group('compose')]
compose-registry-watcher-cli *args: _touch-docker-env compose-cli-migrate
  just _compose-cli registry-watcher-cli {{ args }}

# Update last seen reference to the current index head, to only build newly published crates
[group('compose')]
compose-cli-queue-head: 
  just compose-registry-watcher-cli queue set-last-seen-reference --head

# run migrations, then launch one or more docker compose profiles in the background
[group('compose')]
compose-up *profiles: _touch-docker-env compose-cli-migrate
  docker compose {{ prepend("--profile ", profiles) }} up --build -d --wait --remove-orphans

# Launch web server in the background
[group('compose')]
compose-up-web: 
  just compose-up web

# Launch two build servers in the background 
[group('compose')]
compose-up-builder: 
  just compose-up builder

# Launch registry watcher in the background 
[group('compose')]
compose-up-watcher: 
  just compose-up watcher

# Launch prometheus server in the background 
[group('compose')]
compose-up-metrics: 
  just compose-up metrics

# Launch everything, all at once, in the background
[group('compose')]
compose-up-full:
  just compose-up full

# Shutdown docker services, keep containers & volumes alive.
[group('compose')]
compose-down:
  docker compose --profile full down --remove-orphans


# Shutdown docker services, then
# clean up docker images, volumes & other local artifacts from 
# this docker-compose project
[group('compose')]
compose-down-and-wipe:
  docker compose --profile full down --volumes --remove-orphans --rmi local
  rm -rf ignored/ && mkdir -p ignored


# stream logs from all services running in docker-compose. Optionally specify services to tail logs from.
[group('compose')]
compose-logs *services:
  docker compose --profile full logs -f {{ services }}
