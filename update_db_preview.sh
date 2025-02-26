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
    rm -rf "$TEMP_DB_RENDER_DIR" &>/dev/null || true
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
  panic "Failed to create Docker network '$NETWORK_NAME'."
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
  panic "Failed to start PostgreSQL container."
fi

log "Waiting for PostgreSQL to be ready..."
TIMEOUT=30
SECONDS_ELAPSED=0
until docker exec "$POSTGRES_CONTAINER_ID" pg_isready -U "$POSTGRES_USER" >/dev/null 2>&1; do
  sleep 1
  SECONDS_ELAPSED=$((SECONDS_ELAPSED + 1))
  if [ "$SECONDS_ELAPSED" -ge "$TIMEOUT" ]; then
    panic "Timed out waiting for PostgreSQL to be ready."
  fi
done

HOST_POSTGRES_PORT="$(docker port "$POSTGRES_CONTAINER_ID" 5432 | sed 's/.*://')"
if [ -z "$HOST_POSTGRES_PORT" ]; then
  panic "Failed to retrieve the host port for PostgreSQL."
fi

if ! command -v sqlx >/dev/null 2>&1; then
  panic "sqlx is not installed. Please install sqlx to proceed."
fi

log "Running migrations..."
DATABASE_URL="postgres://$POSTGRES_USER:$POSTGRES_PASSWORD@localhost:$HOST_POSTGRES_PORT/$POSTGRES_DB"
(cd "$SCRIPT_DIR/packages/adapters/storage" && sqlx migrate run --database-url "$DATABASE_URL" &>/dev/null) || {
  panic "sqlx migration failed."
}

log "Running SchemaSpy..."

# Ensure the SchemaSpy output directory is writable by all
chmod 777 "$TEMP_DB_RENDER_DIR"

SCHEMASPY_LOG="$SCRIPT_DIR/schemaspy.log"
docker run --rm \
  --network "$NETWORK_NAME" \
  -v "$TEMP_DB_RENDER_DIR:/output" \
  schemaspy/schemaspy \
  -noimplied \
  -u "$POSTGRES_USER" \
  -t pgsql \
  -host "$POSTGRES_CONTAINER_NAME" \
  -db "$POSTGRES_DB" \
  -p "$POSTGRES_PASSWORD" &>"$SCHEMASPY_LOG" || {
  log "$(cat "$SCHEMASPY_LOG")"
  panic "SchemaSpy failed. Check the log at $SCHEMASPY_LOG"
}

mkdir -p "$DOTS_DIR/tables"
mkdir -p "$PNGS_DIR/tables"

# Enable nullglob to handle cases with no matching files
shopt -s nullglob

log "Saving rendered files..."

# Copy summary diagram files
cp "$TEMP_DB_RENDER_DIR/diagrams/summary/relationships.real.large.dot" "$DOTS_DIR/relationships.dot"
cp "$TEMP_DB_RENDER_DIR/diagrams/summary/relationships.real.large.png" "$PNGS_DIR/relationships.png"

# Copy table DOT and PNG files:
# First, copy over all .1degree files, then overwrite with any .2degrees files.
for file in "$TEMP_DB_RENDER_DIR/diagrams/tables/"*.1degree.dot; do
  if [ -f "$file" ]; then
    base_name="$(basename "$file" .1degree.dot)"
    cp "$file" "$DOTS_DIR/tables/${base_name}.dot"
  fi
done
for file in "$TEMP_DB_RENDER_DIR/diagrams/tables/"*.2degrees.dot; do
  if [ -f "$file" ]; then
    base_name="$(basename "$file" .2degrees.dot)"
    cp "$file" "$DOTS_DIR/tables/${base_name}.dot"
  fi
done

for file in "$TEMP_DB_RENDER_DIR/diagrams/tables/"*.1degree.png; do
  if [ -f "$file" ]; then
    base_name="$(basename "$file" .1degree.png)"
    cp "$file" "$PNGS_DIR/tables/${base_name}.png"
  fi
done
for file in "$TEMP_DB_RENDER_DIR/diagrams/tables/"*.2degrees.png; do
  if [ -f "$file" ]; then
    base_name="$(basename "$file" .2degrees.png)"
    cp "$file" "$PNGS_DIR/tables/${base_name}.png"
  fi
done

# Disable nullglob after use
shopt -u nullglob

# --- Append orphans ---
# Assume that SchemaSpy produced an orphans DOT file at:
ORPHANS_DOT="$TEMP_DB_RENDER_DIR/diagrams/orphans/orphans.dot"

if [ -f "$ORPHANS_DOT" ]; then
  log "Appending orphan tables from $ORPHANS_DOT into relationships DOT..."

  # Read the orphans DOT file and remove its header and final brace
  ORPHANS_CONTENT=$(sed '1d;$d' "$ORPHANS_DOT")

  # Create a temporary file for the updated relationships DOT.
  TEMP_REL="$(mktemp)"

  # Remove the last line (the closing brace) from the relationships DOT file.
  sed '$d' "$DOTS_DIR/relationships.dot" >"$TEMP_REL"

  # Append a newline, the orphan content, and a newline.
  echo "" >>"$TEMP_REL"
  echo "$ORPHANS_CONTENT" >>"$TEMP_REL"
  echo "" >>"$TEMP_REL"

  # Append the closing brace.
  echo "}" >>"$TEMP_REL"

  # Replace the original relationships DOT with the updated file.
  mv "$TEMP_REL" "$DOTS_DIR/relationships.dot"

  log "Appended orphan nodes into relationships DOT."
else
  log "Orphans dot file not found at $ORPHANS_DOT. Skipping orphan addition."
fi

# --- Fix image paths ---
# SchemaSpy's DOT files reference images using a relative path ../../images.
# If the images directory exists in the SchemaSpy output, update the DOT file to use its absolute path.
IMAGES_DIR="$TEMP_DB_RENDER_DIR/images"
if [ -d "$IMAGES_DIR" ]; then
  ABS_IMAGES_DIR=$(realpath "$IMAGES_DIR")
  log "Updating image paths in relationships DOT to use absolute path: $ABS_IMAGES_DIR"
  sed -i "s|\.\./\.\./images|$ABS_IMAGES_DIR|g" "$DOTS_DIR/relationships.dot"
fi

# Re-render the relationships diagram PNG using the updated DOT file.
log "Re-rendering relationships diagram to PNG..."
dot -Tpng "$DOTS_DIR/relationships.dot" -o "$PNGS_DIR/relationships.png"
log "Browse rendered files in $DB_PREVIEW_DIR"
