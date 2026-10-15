# Rewrites .sqlx/, the query metadata that lets the workspace build without a database.
#
# Run it after changing a query or a migration, and commit the result.
# With -Check it changes nothing and fails if .sqlx/ is out of date, as CI does.
#
# Needs Docker, a .env copied from .env.example, and sqlx-cli:
#   cargo install sqlx-cli --version 0.9.0 --no-default-features --features postgres
#
# An applied migration must not be edited: add a new one instead. If you did edit one
# that only your own database has seen, start over with
#   cargo sqlx database reset --source crates/routeforge-domain/migrations
param([switch]$Check)

$ErrorActionPreference = 'Stop'

Push-Location (Join-Path $PSScriptRoot '..')
try {
    # A failing program does not stop a PowerShell script by itself, so each step
    # runs only if the one before it succeeded.
    docker compose up -d --wait db
    if ($LASTEXITCODE -eq 0) {
        cargo sqlx migrate run --source crates/routeforge-domain/migrations
    }
    if ($LASTEXITCODE -eq 0) {
        if ($Check) {
            cargo sqlx prepare --workspace --check '--' --all-targets
        } else {
            cargo sqlx prepare --workspace '--' --all-targets
        }
    }
} finally {
    Pop-Location
}
exit $LASTEXITCODE
