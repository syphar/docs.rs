# Run a docs.rs CLI in a one-off Docker Compose container. Normal CLI recipes
# run on the host; this is the explicit fallback for containerized development.
[group('cli')]
[group('compose')]
docker-cli command *args: _touch-docker-env _ensure_db_and_s3_are_running
    #!/usr/bin/env bash
    set -euo pipefail

    case "{{ command }}" in
      admin)
        service="cli"
        ;;
      builder)
        service="builder-cli"
        ;;
      watcher)
        service="registry-watcher-cli"
        ;;
      *)
        echo "Unknown Docker CLI: {{ command }}" >&2
        echo "Expected one of: admin, builder, watcher" >&2
        exit 2
        ;;
    esac

    # `docker compose run` ignores service dependencies, so the recipe starts
    # the database and object storage explicitly before reaching this point.
    docker compose run --build --rm "$service" {{ args }}

# Run any admin CLI command on the host. Args are passed to the binary.
[group('cli')]
cli +args: cli-db-migrate
    cargo run -p docs_rs_admin -- {{ args }}

# Initialize the `docs.rs` database
[group('cli')]
[group('database')]
cli-db-migrate version="": _ensure_db_and_s3_are_running
    #!/usr/bin/env bash
    set -euo pipefail

    # Intentionally not using `cli`, because it depends on `cli-db-migrate`.
    # Otherwise we would have a stack overflow / infinite recursion.
    export SQLX_OFFLINE=1
    cargo run -p docs_rs_admin -- database migrate {{ version }}

# add a release to the build queue
[group('cli')]
[group('queue')]
cli-queue-add crate_name crate_version:
    # This only needs the database, so it can use the admin CLI.
    just cli queue add {{ crate_name }} {{ crate_version }}

# Run any builder CLI command on the host.
[group('builder')]
[group('cli')]
cli-build +args: cli-db-migrate
    cargo run -p docs_rs_builder -- {{ args }}

# set the nightly rust version to be used for builds. Format: `nightly-YYYY-MM-DD`
# or just `nightly` for always using the latest nightly.
[group('builder')]
[group('cli')]
cli-build-set-toolchain name only_first_time="false":
    #!/usr/bin/env bash
    set -euo pipefail

    FLAG=""
    if [ "{{ only_first_time }}" = "true" ]; then FLAG="--only-first-time"; fi
    just cli-build build set-toolchain {{ name }} $FLAG

# update the toolchain in the builders
[group('builder')]
[group('cli')]
cli-build-update-toolchain:
    just cli-build build update-toolchain

# build & upload toolchain shared static resources
[group('builder')]
[group('cli')]
cli-build-add-essential-files:
    just cli-build build add-essential-files

# build a release
[group('builder')]
[group('cli')]
cli-build-crate name version:
    just cli-build build crate {{ name }} {{ version }}

# Run any registry-watcher CLI command on the host.
[group('cli')]
[group('watcher')]
cli-watcher +args: cli-db-migrate
    cargo run -p docs_rs_watcher -- {{ args }}

# Update last seen reference to the given hash, or the current `HEAD`.
[group('cli')]
[group('queue')]
cli-queue-reset-last-seen-ref ref="--head":
    just cli-watcher queue set-last-seen-reference {{ ref }}

# find differences between crates.io and our own database, and fix them on our side.
[group('cli')]
[group('database')]
cli-db-synchronize *args:
    just cli-watcher database synchronize {{ args }}
