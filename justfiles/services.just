_touch-docker-env:
  touch .docker.env

_ensure_db_and_s3_are_running:
  # dependencies in the docker-cli file are ignored 
  # here. Instead we explicitly start any dependent services first.
  docker compose up -d db s3 --wait

# run migrations, then launch one or more docker compose profiles in the background
[group('compose')]
compose-up *profiles: _touch-docker-env cli-db-migrate
  docker compose {{ prepend("--profile ", profiles) }} up --build -d --wait --remove-orphans

# Launch web server in the background
[group('compose')]
compose-up-web: 
  just compose-up web

# Launch two build servers in the background 
[group('compose')]
compose-up-builder: 
  just compose-up builder

# Launch registry watcher in the background 
[group('compose')]
compose-up-watcher: 
  just compose-up watcher

# Launch prometheus server in the background 
[group('compose')]
compose-up-metrics: 
  just compose-up metrics

# Launch everything, all at once, in the background
[group('compose')]
compose-up-full:
  just compose-up full

# Shutdown docker services, keep containers & volumes alive.
[group('compose')]
compose-down:
  docker compose --profile full down --remove-orphans

# Shutdown docker services, then clean up docker images, volumes & other local artifacts from this docker-compose project
[group('compose')]
compose-down-and-wipe:
  docker compose --profile full --profile manual down --volumes --remove-orphans --rmi local
  rm -rf ignored/ && mkdir -p ignored

# stream logs from all services running in docker-compose. Optionally specify services to tail logs from.
[group('compose')]
compose-logs *services:
  docker compose --profile full logs -f {{ services }}
