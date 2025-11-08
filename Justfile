set shell := ["bash", "-Eeuo", "pipefail", "-c"]
set ignore-comments
set dotenv-load := true
set dotenv-override := true

# some environment variables that are needed for _local_ commands
# these are defaults that can be overwritten in your local .env file
export DOCSRS_INCLUDE_DEFAULT_TARGETS := env("DOCSRS_INCLUDE_DEFAULT_TARGETS", "false")
export DOCSRS_LOG := env("DOCSRS_LOG", "docs_rs=debug,rustwide=info")
export RUST_BACKTRACE := env("RUST_BACKTRACE", "1")
export DOCSRS_PREFIX := env("DOCSRS_PREFIX", "ignored/cratesfyi-prefix")

# database & s3 settings for local testing, to connect to the DB & S3/minio instance 
# configured in docker-compose.yml
export DOCSRS_DATABASE_URL := env("DOCSRS_DATABASE_URL", "postgresql://cratesfyi:password@localhost:15432")
export AWS_ACCESS_KEY_ID := env("AWS_ACCESS_KEY_ID", "cratesfyi")
export AWS_SECRET_ACCESS_KEY := env("AWS_SECRET_ACCESS_KEY", "secret_key")
export S3_ENDPOINT := env("S3_ENDPOINT", "http://localhost:9000")


# List available commands
_default:
    just --list

import 'justfiles/cli.just'
import 'justfiles/utils.just'
import 'justfiles/services.just'
import 'justfiles/testing.just'

psql:
  psql $DOCSRS_DATABASE_URL
