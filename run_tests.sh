#!/usr/bin/env bash

while true; do
	case "$1" in
	--logs)
		show_logs="true"
		shift 1
		;;
	-- | "")
		break
		;;
	*)
		printf "Unknown option %s\n" "$1"
		exit 1
		;;
	esac
done

cargo test --workspace --exclude e2e

docker compose up -d
trap 'docker compose down > /dev/null 2>&1' EXIT

cargo test --package e2e -- --nocapture

if [[ $show_logs = "true" ]]; then
	docker compose logs -f block_committer &
fi
