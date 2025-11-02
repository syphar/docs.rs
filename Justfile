set shell := ["bash", "-Eeuo", "pipefail", "-c"]

# List available commands
_default:
    just --list

import 'justfiles/cli.just'
import 'justfiles/services.just'
import 'justfiles/testing.just'
