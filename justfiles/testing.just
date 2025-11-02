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
  
run-gui-tests: _ensure_db_and_s3_are_running cli-db-migrate compose-up-web
  # We add the information we need.
  # just cli-build-update-toolchain
  # just cli-build-crate sysinfo 0.23.4
  # just cli-build-crate sysinfo 0.23.5
  # just cli-build-crate libtest 0.0.1
  # just cli-build-add-essential-files

  just _cli gui_tests 

