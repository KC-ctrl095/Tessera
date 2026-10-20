#!/bin/sh
# Deploys the current release of the shop app to this server.
# Usage: bin/deploy.sh [release-tag]
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
tag=${1:-latest}

. "$here/config/app.conf"
echo "deploying release $tag to $APP_DIR"
mkdir -p "$APP_DIR/releases/$tag"
ln -sfn "$APP_DIR/releases/$tag" "$APP_DIR/current"
echo "release $tag is live"
