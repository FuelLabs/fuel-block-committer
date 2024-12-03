#!/usr/bin/env bash

# Drop the database
docker compose -f sql-compose.yaml down --volumes
docker compose -f sql-compose.yaml up -d
(cd ./packages/adapters/storage && sqlx migrate run)
