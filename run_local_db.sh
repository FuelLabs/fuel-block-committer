#!/usr/bin/env bash

# Drop the database
docker compose -f sql-compose.yaml down --volumes
docker compose -f sql-compose.yaml up -d
( cd ./packages/storage && sqlx migrate run)
