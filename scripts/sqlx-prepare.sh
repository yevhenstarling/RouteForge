#!/usr/bin/env sh
# Rewrites .sqlx/, the query metadata that lets the workspace build without a database.
#
# Run it after changing a query or a migration, and commit the result.
# With --check it changes nothing and fails if .sqlx/ is out of date, as CI does.
#
# Needs Docker, a .env copied from .env.example, and sqlx-cli:
#   cargo install sqlx-cli --version 0.9.0 --no-default-features --features postgres
#
# An applied migration must not be edited: add a new one instead. If you did edit one
# that only your own database has seen, start over with
#   cargo sqlx database reset --source crates/routeforge-domain/migrations
set -eu

cd "$(dirname "$0")/.."

docker compose up -d --wait db
cargo sqlx migrate run --source crates/routeforge-domain/migrations
cargo sqlx prepare --workspace "$@" -- --all-targets
