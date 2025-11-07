set shell := ["bash", "-Eeuo", "pipefail", "-c"]
set ignore-comments

# List available commands
_default:
    just --list

import 'justfiles/cli.just'
import 'justfiles/utils.just'
import 'justfiles/services.just'
import 'justfiles/testing.just'
