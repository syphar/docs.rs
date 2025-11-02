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

# clean up docker images, volumes & other local artifacts from this docker-compose 
# config
[group('compose')]
cleanup:
  docker compose down --volumes --remove-orphans --rmi local
  rm -rf .rustwide-docker/ && mkdir -p .rustwide-docker
  rm -rf ignored/ && mkdir -p ignored


# run any CLI command in its own one-off docker container. Args are passed to the container.
[group('compose')]
compose-cli *args: _touch-docker-env
  # ensure dependencies are running.
  # docker compose ignores dependencies from the yaml file 
  # when we use `compose run`.
  # 
  # no-op if they are already running.
  docker compose up -d db s3 --wait
  # run the CLI with the provided args.

  # not suited for commands that need: 
  # * the crates.io index, or
  # * need to run builds.
  docker compose run --build --rm cli {{ args }}

# run builder CLI command in its own one-off docker container.
[group('compose')]
compose-builder-cli *args: _touch-docker-env
  docker compose up -d db s3 --wait
  docker compose run --build --rm builder-cli {{ args }}

# run registry-watcher CLI command in its own one-off docker container.
[group('compose')]
compose-registry-watcher-cli *args: _touch-docker-env
  docker compose up -d db s3 --wait
  docker compose run --build --rm registry-watcher-cli {{ args }}

# Initialize the docker compose database
[group('compose')]
compose-cli-migrate: 
  just compose-cli database migrate

# Update last seen reference to the current index head, to only build newly published crates
[group('compose')]
compose-cli-queue-head: 
  just compose-cli queue set-last-seen-reference --head

# run migrations, then launch one or more docker compose profiles in the background
[group('compose')]
compose-up *profiles: _touch-docker-env compose-cli-migrate
  if [ {{profiles.len()}} -eq 0 ]; then
    echo "❌ Error: You must specify at least one profile, e.g.:";
    echo "   just compose-up web";
    echo "   just compose-up web builder";
    exit 1;
  fi

 docker compose \
    {{ profiles | map(p => "--profile " + p) | join(" ") }} \
    up --build -d

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

# Launch everything in the background
[group('compose')]
compose-up-full:
  just compose-up full

# Shutdown docker services and cleanup all temporary volumes
[group('compose')]
compose-down:
  docker compose --profile full down --remove-orphans
