#!/usr/bin/env bash
# Run the deployed admin CLI without requiring local database access.
set -euo pipefail

if (( $# < 2 )); then
    echo "Usage: $0 <staging|prod> <admin arguments...>" >&2
    exit 2
fi
case "$1" in
    staging|prod) environment=$1 ;;
    *) echo 'Environment must be staging or prod' >&2; exit 2 ;;
esac
shift

# The Justfile exports credentials for local S3 testing. Use the selected real
# AWS profile instead; never pass those local credentials to AWS.
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_SECURITY_TOKEN
export AWS_PROFILE="docs-rs-$environment" AWS_REGION=us-east-1 AWS_DEFAULT_REGION=us-east-1 AWS_PAGER=""
cluster="docs-rs-$environment"
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

aws ecs describe-services --cluster "$cluster" --services docs-rs-web --output json |
    jq -e '{awsvpcConfiguration: .services[0].networkConfiguration.awsvpcConfiguration}
        | select(.awsvpcConfiguration.subnets | length > 0)' > "$workdir/network.json"
jq -n --args '{containerOverrides: [{name: "app", command: $ARGS.positional}]}' -- "$@" > "$workdir/overrides.json"
aws ecs run-task --cluster "$cluster" --task-definition docs-rs-admin \
    --launch-type FARGATE --network-configuration "file://$workdir/network.json" \
    --overrides "file://$workdir/overrides.json" --output json > "$workdir/launch.json"
if ! task=$(jq -er 'select((.failures | length) == 0) | .tasks[0].taskArn // empty' "$workdir/launch.json"); then
    cat "$workdir/launch.json" >&2
    exit 1
fi
echo "Started $task"
echo 'Waiting for completion; interrupting this script does not stop the remote task.'
aws ecs wait tasks-stopped --cluster "$cluster" --tasks "$task"
aws ecs describe-tasks --cluster "$cluster" --tasks "$task" --output json > "$workdir/task.json"

# The shared task module uses group /ecs/<task family> and stream app/app/<task ID>.
if aws logs get-log-events --log-group-name /ecs/docs-rs-admin \
    --log-stream-name "app/app/${task##*/}" --start-from-head --output json > "$workdir/logs.json"; then
    jq -r '.events[].message' "$workdir/logs.json"
else
    echo 'Could not fetch logs; check CloudWatch /ecs/docs-rs-admin.' >&2
fi
if ! code=$(jq -er '.tasks[0].containers[] | select(.name == "app") | .exitCode // empty' "$workdir/task.json"); then
    jq '{failures, tasks: [.tasks[] | {stopCode, stoppedReason, containers}]}' "$workdir/task.json" >&2
    exit 1
fi
exit "$code"
