# PR desc

- the `Justfile` commands are built so they would even work on a 100% fresh
  setup, and will set up and initialize everything they need.
- I removed `index: Index` from the context. There is no need to clone the
  crates.io index on a web- or build-server. The handful of places where we then
  still need the index just create the obj.
- there was an error I had when calling `Index::peek_changes` from
  `crates-index-diff`. In there, we're using the github fastpath to check if
  there are new commits in the repo, without having to actually `git pull` from
  the remote. When I called `.peek_changes` in an async context, this lead to
  tokio errors because inside `crates-index-diff` we're using
  `reqwest::blocking`. Odd thing is: I couldn't find out why this doesn't fail
  on production. It might have started failing just after the config/context
  rewrite, which is not deployed yet.
  [#2937](https://github.com/rust-lang/docs.rs/pull/2937)
- unsure if the prefix mount should also be a docker volume, for performance.
  Not sure how often we actually have to look at the contents?

- no apt-get upgrade in docker build, typically you should rely on the
  base-image to be up-to-date. Rebuild every night.
- focus right now: docker images for production and local manual testing
- not yet: running build-tests locally

## TODO

- what is better, buildx cache mounts or just layer caching?

  ## docker image

for now, these are production images, changing files will not auto-reload /
build the image. We could decide to do that layer. Also I don't want to copy the
".git" folder into the image, just for the version number. I made the SHA a
build-arg / env and used these in our codebase.

The fallback to fetching the has from the repo still exists, we might be able to
drop this functionality at some point.

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
