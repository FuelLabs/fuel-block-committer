#!/usr/bin/env bash
set -e

err() {
	echo -e "\e[31m\e[1merror:\e[0m $@" 1>&2
}

status() {
	WIDTH=12
	printf "\e[32m\e[1m%${WIDTH}s\e[0m %s\n" "$1" "$2"
}
# install dasel
curl -sSLf "https://github.com/TomWright/dasel/releases/download/v1.24.3/dasel_linux_amd64" -L -o dasel
chmod +x dasel
mv ./dasel /usr/local/bin/dasel
# check appVersion with crate package metadata
HELM_APP_VERSION=$(cat helm/fuel-block-committer/Chart.yaml | dasel -r yaml 'appVersion')
CRATE_VERSION=$(cat Cargo.toml | dasel -r toml 'workspace.package.version')
if [ "$HELM_APP_VERSION" != "$CRATE_VERSION" ]; then
	err "crate version $CRATE_VERSION, doesn't match helm app version $HELM_APP_VERSION"
	exit 1
else
	status "crate version matches helm chart app version $HELM_APP_VERSION"
fi
