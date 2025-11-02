# PR desc

- the `Justfile` commands are built so they would even work on a 100% fresh
  setup, and will set up and initialize everything they need.

  ## docker image

for now, these are production images, changing files will not auto-reload /
build the image. We could decide to do that layer.

## profiles

- default: just db & s3

runs & configures by default:

- `db` -> postgres db
- `s3` -> minio

optional profile: `web`:

- `web` -> webserver

optional profile: `builder`:

- `builder-a` -> build-server 1
- `builder-b` -> build-server 2 ( two parallel build-servers, sharing nothing
  apart from the build queue they access)

optional profile: `watcher`:

- `registry-watcher` ->

* crates.io registry watcher
* repo-stats updater
* cdn invalidator
* release-rebuild-enqueuer

optional profile: `metrics`:

- `prometheus` -> configured prometheus instance

optional profile: `full`: all of the above.

Services purely for manual usage with `docker compose run` are:

- `cli`: to run simple CLI commands that only need the database & S3
- `builder-cli`: to run CLI commands that need the build environment.
- `registry-watcher-cli`: to run CLI commands that need the crates.io index.

CAVEATS:

- the build-servers have to run on the `linux/amd64` platform, while it doesn't
  matter for the rest of the services. This means for example on a Mac, the
  layers will be cached separately, once for `linux/amd64` and once for
  `linux/arm64`. Only alternative would be to build everything for `amd64`, but
  that would imply a performance impact on the services that don't need it.
- volumes: typically docker-native volumes are faster than mounts, but sometimes
  annoying to inspect for debugging.

For now we choose:

- docker-native for DB, S3, rustwide workspace, crates.io index
- mounts for prefix, code mounts
- prometheus scrape config is set to collect from the web server, the registry
  watcher, and the build servers. Scraping is not dynamic, so the local
  prometheus server will try to fetch from all service instances (web, watcher,
  builder), and just error in case the specific server isn't accessible.
