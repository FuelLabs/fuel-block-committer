#!/bin/sh
set -e

PORT="9545"
IP="0.0.0.0"

# Deployment needs to be done every time the container starts
if [[ -f /contracts_deployed ]]; then
	rm /contracts_deployed
fi

# Functions

replace_placeholders() {
	KEY="$1"
	URL="$2"
	if [[ -z "$KEY" ]] || [[ -z "$URL" ]]; then
		echo "usage: replace_placeholders <key> <url>"
		exit 1
	fi

	sed -i "s/KEY/$KEY/g;s#URL#$URL#g" ./hardhat.config.ts
}

spawn_eth_node() {
	IP="$1"
	PORT="$2"
	if [[ -z "$IP" ]] || [[ -z "$PORT" ]]; then
		echo "usage: spawn_eth_node <ip> <port>"
		exit 1
	fi

	pnpm hardhat node --network hardhat --port "$PORT" --hostname "$IP" &
	trap "kill $! 2>/dev/null" EXIT
}

wait_node_healthy() {
	URL="$1"
	if [[ -z "$URL" ]]; then
		echo "usage: wait_node_healthy <url>"
		exit 1
	fi

	curl \
		--fail \
		--silent \
		-H "Content-Type: application/json" \
		--retry-connrefused \
		--retry 120 \
		--retry-delay 1 \
		-d '{"jsonrpc":"2.0","id":0,"method":"net_version","params":[]}' \
		"$URL" >/dev/null
}

deploy_contracts() {
	KEY="$1"
	URL="$2"
	if [[ -z "$KEY" ]] || [[ -z "$URL" ]]; then
		echo "usage: deploy_contracts <key> <url>"
		exit 1
	fi
	LOCALHOST_HTTP="$URL" DEPLOYER_KEY="$KEY" pnpm run node-deploy
}

usage() {
	echo "Usage: $0 --key KEY [options]"
	echo "  --key KEY    The private key to use for deployment"
	echo "  -p, --port   Set the port number (default: ${PORT})"
	echo "  -i, --ip     Set the IP address (default: ${IP})"
	echo "  -h, --help   Display this help message"
}

# Main

while [[ "$#" -gt 0 ]]; do
	case $1 in
	--key)
		KEY="$2"
		shift
		;;
	-p | --port)
		PORT="$2"
		shift
		;;
	-i | --ip)
		IP="$2"
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "Unknown parameter passed: $1"
		usage
		exit 1
		;;
	esac
	shift
done

if [[ -z "$KEY" ]]; then
	echo "Error: KEY is required."
	usage
	exit 1
fi

URL="http://$IP:$PORT"
replace_placeholders "$KEY" "$URL"
spawn_eth_node "$IP" "$PORT"

wait_node_healthy "$URL"

deploy_contracts "$KEY" "$URL"
touch /contracts_deployed

wait
