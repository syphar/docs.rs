# Run the deployed admin CLI: just admin staging queue add serde 1.0.228
[group('deployment')]
[positional-arguments]
admin environment +args:
    bash scripts/admin.sh "$@"
