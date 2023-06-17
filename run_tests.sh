#!/bin/bash -e

while true; do
  case "$1" in
    --logs)
      show_logs="true"
      shift 1;;
    --| "")
      break;;
     *)
      printf "Unknown option %s\n" "$1"
      exit 1;;
  esac
done

cargo test --bin fuel-block-committer

docker compose up -d
trap 'docker compose down' EXIT

cargo test --test e2e -- --nocapture

if [[ $show_logs = "true" ]]; then
    docker compose logs block_committer
fi
