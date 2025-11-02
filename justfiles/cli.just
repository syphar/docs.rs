# a collection of just commands to wrap various docs.rs CLI commands, 
# and run then in a once-off docker container.
# _Which_ container depends on the command itself and its dependencies.

# low-level helper to run any CLI command in its own one-off docker container, 
# ensuring that `db` and `s3` are running.
_cli service_name *args: _touch-docker-env
  # dependencies in the docker-cli file are ignored 
  # here. Instead we explicitly start any dependent services first.
  docker compose up -d db s3 --wait
  # run the CLI command
  docker compose run --build --rm {{ service_name }} {{ args }}

# run any CLI command in its own one-off `cli` docker container. Args are passed to the container.
# Only for commands that just need `db` and `s3` and minimal dependencies.
[group('cli')]
cli +args: _touch-docker-env  cli-db-migrate
  just _cli cli {{ args }}

# Initialize the `docs.rs` database
[group('cli')]
cli-db-migrate: 
  # intentially not using `cli`, because it has a dependency on `cli-db-migrate`. 
  # Otherwise we would have a stack overflow / infinite recursion.
  # 
  # TODO: potential optimization: only run the container when we have to 
  # run migrations?
  just _cli cli database migrate

# add a release to the build queue
[group('cli')]
cli-queue-add +args:
  # only does things with the database, so can use the lightweight `cli` container.
  just cli queue add {{ args }}

# run builder CLI command in its own one-off `build-server` docker container.
# Uses a separate builder-cli container & workspace.
[group('cli')]
cli-build +args: _touch-docker-env cli-db-migrate
  just _cli builder-cli {{ args }}

# set the nightly rust version to be used for builds. Format: `nightly-YYYY-MM-DD`
[group('cli')]
cli-build-set-toolchain name only_first_time="false":
  FLAG=""
  if [ "{{only_first_time}}" = "true" ]; then FLAG="--only-first-time"; fi
  just cli-build build set-toolchain {{ name }} $FLAG

# update the toolchain in the builders
[group('cli')]
cli-build-update-toolchain:
  just cli-build build update-toolchain

# build & upload toolchain shared static resources
[group('cli')]
cli-build-add-essential-files:
  just cli-build build add-essential-files

# build a release
[group('cli')]
cli-build-crate name version:
  just cli-build build crate {{ name }} {{ version }}

# run registry-watcher CLI command in its own one-off `registry-watcher` docker container.
[group('cli')]
cli-watcher +args: _touch-docker-env cli-db-migrate
  just _cli registry-watcher-cli {{ args }}

# Update last seen reference to the given hash, or the current `HEAD`. 
[group('cli')]
cli-queue-reset-last-seen-ref ref="--head": 
  just cli-watcher queue set-last-seen-reference {{ ref }}

# synchronize the crates.io index with our own database. 
[group('cli')]
cli-db-synchronize +args:
  just cli-watcher database synchronize {{ args }}
