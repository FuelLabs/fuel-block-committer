#!/bin/bash

set -euo pipefail # Exit on error, unset variables, and pipeline failures

cleanup() {
  EXIT_STATUS=$? # Capture the exit status of the script

  if [[ $EXIT_STATUS -eq 0 ]] && declare -p SCHEMASPY_LOG &>/dev/null && [[ -f "$SCHEMASPY_LOG" ]]; then
    rm -f "$SCHEMASPY_LOG"
  fi

  if declare -p POSTGRES_CONTAINER_ID &>/dev/null && [[ -n "$POSTGRES_CONTAINER_ID" ]]; then
    docker rm -f "$POSTGRES_CONTAINER_ID" >/dev/null 2>&1 || true
  fi
  if declare -p NETWORK_ID &>/dev/null && [[ -n "$NETWORK_ID" ]]; then
    docker network rm "$NETWORK_ID" >/dev/null 2>&1 || true
  fi
  if declare -p TEMP_DB_RENDER_DIR &>/dev/null && [[ -d "$TEMP_DB_RENDER_DIR" ]]; then
    rm -rf "$TEMP_DB_RENDER_DIR"
  fi
}

panic() {
  echo "Error: $1" 1>&2
  exit 1
}

log() {
  echo "$@" 1>&2
}

trap cleanup EXIT # Ensure cleanup on exit

# Get the directory of the script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Check for required commands
for cmd in docker sqlx sed; do
  if ! command -v $cmd >/dev/null 2>&1; then
    panic "Command '$cmd' is required but not installed."
  fi
done

# Variables
TIMESTAMP=$(date +%s%N)
NETWORK_NAME="temp_network_$TIMESTAMP"
POSTGRES_CONTAINER_NAME="temp_postgres_$TIMESTAMP"
TEMP_DB_RENDER_DIR="$(mktemp -d)"
DB_PREVIEW_DIR="$SCRIPT_DIR/db_preview"
DOTS_DIR="$DB_PREVIEW_DIR/dots"
PNGS_DIR="$DB_PREVIEW_DIR/pngs"
POSTGRES_USER="${POSTGRES_USER:-username}"
POSTGRES_PASSWORD="${POSTGRES_PASSWORD:-password}"
POSTGRES_DB="${POSTGRES_DB:-test}"

NETWORK_ID="$(docker network create "$NETWORK_NAME" 2>/dev/null)"
if [ -z "$NETWORK_ID" ]; then
  panic "Failed to create Docker network '$NETWORK_NAME'." 1>&2
fi

# Start PostgreSQL container
log "Starting PostgreSQL container..."
POSTGRES_CONTAINER_ID="$(docker run -d \
  --name "$POSTGRES_CONTAINER_NAME" \
  --network "$NETWORK_NAME" \
  -e POSTGRES_USER="$POSTGRES_USER" \
  -e POSTGRES_PASSWORD="$POSTGRES_PASSWORD" \
  -e POSTGRES_DB="$POSTGRES_DB" \
  -p 127.0.0.1::5432 \
  postgres:latest 2>/dev/null)"

if [ -z "$POSTGRES_CONTAINER_ID" ]; then
  panic "Failed to start PostgreSQL container." 1>&2
fi

log "Waiting for PostgreSQL to be ready..."
TIMEOUT=30
SECONDS_ELAPSED=0
until docker exec "$POSTGRES_CONTAINER_ID" pg_isready -U "$POSTGRES_USER" >/dev/null 2>&1; do
  sleep 1
  SECONDS_ELAPSED=$((SECONDS_ELAPSED + 1))
  if [ "$SECONDS_ELAPSED" -ge "$TIMEOUT" ]; then
    panic "Timed out waiting for PostgreSQL to be ready." 1>&2
  fi
done

HOST_POSTGRES_PORT="$(docker port "$POSTGRES_CONTAINER_ID" 5432 | sed 's/.*://')"
if [ -z "$HOST_POSTGRES_PORT" ]; then
  panic "Failed to retrieve the host port for PostgreSQL." 1>&2
fi

if ! command -v sqlx >/dev/null 2>&1; then
  panic "sqlx is not installed. Please install sqlx to proceed." 1>&2
fi

log "Running migrations..."
DATABASE_URL="postgres://$POSTGRES_USER:$POSTGRES_PASSWORD@localhost:$HOST_POSTGRES_PORT/$POSTGRES_DB"
(cd "$SCRIPT_DIR/packages/storage" && sqlx migrate run --database-url "$DATABASE_URL" &>/dev/null) || {
  panic "sqlx migration failed." 1>&2
}

log "Running SchemaSpy..."
SCHEMASPY_LOG="$SCRIPT_DIR/schemaspy.log"
docker run --rm \
  --network "$NETWORK_NAME" \
  -v "$TEMP_DB_RENDER_DIR:/output" \
  schemaspy/schemaspy \
  -u "$POSTGRES_USER" \
  -t pgsql \
  -host "$POSTGRES_CONTAINER_NAME" \
  -db "$POSTGRES_DB" \
  -p "$POSTGRES_PASSWORD" >"$SCHEMASPY_LOG" 2>&1 || {
  log "$(cat "$SCHEMASPY_LOG")"
  panic "SchemaSpy failed. Check the log at $SCHEMASPY_LOG" 1>&2
}

mkdir -p "$DOTS_DIR/tables"
mkdir -p "$PNGS_DIR/tables"

# Enable nullglob to handle cases with no matching files
shopt -s nullglob

log "Saving rendered files..."
# Copy and rename .dot and .png files
cp "$TEMP_DB_RENDER_DIR/diagrams/summary/relationships.real.large.dot" "$DOTS_DIR/relationships.dot"
cp "$TEMP_DB_RENDER_DIR/diagrams/summary/relationships.real.large.png" "$PNGS_DIR/relationships.png"

DOT_FILES=("$TEMP_DB_RENDER_DIR/diagrams/tables/"*.2degrees.dot)
if [ ${#DOT_FILES[@]} -eq 0 ]; then
  panic "No .2degrees.dot files found." 1>&2
else
  for file in "${DOT_FILES[@]}"; do
    base_name="$(basename "$file" .2degrees.dot)"
    cp "$file" "$DOTS_DIR/tables/${base_name}.dot"
  done
fi

PNG_FILES=("$TEMP_DB_RENDER_DIR/diagrams/tables/"*.2degrees.png)
if [ ${#PNG_FILES[@]} -eq 0 ]; then
  panic "No .2degrees.png files found." 1>&2
else
  for file in "${PNG_FILES[@]}"; do
    base_name="$(basename "$file" .2degrees.png)"
    cp "$file" "$PNGS_DIR/tables/${base_name}.png"
  done
fi

# Disable nullglob after use
shopt -u nullglob
log "Browse rendered files in $DB_PREVIEW_DIR"
