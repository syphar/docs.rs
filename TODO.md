# idea

## phase 1

- add rustdoc_status & doc_targets to builds table, since it might change with
  the build
- finish_release can be more intelligent when updating rustdoc_status &
  doc_targets, not updating it when the build failed, and there is an existing
  successful build?
- then we can start rebuilds again
- run a query to past releases to where we have more than one build, at least
  one is successful, and we have rustdoc_status = false? Is this enough? How to
  exclude binaries / non-libs?

## phase 2

- backfill the new build fields from the releases table
- add calculated summary fields for these in release_build_status
- backfill these too
- drop the fields from releases, use the release_build_status
