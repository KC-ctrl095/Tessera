#!/bin/sh
# Pings the storefront and the API; sends an alert when one of them is down.
for url in https://shop.example.com/ https://api.shop.example.com/health; do
  if ! curl -fsS -m 10 -o /dev/null "$url"; then
    echo "$(date -u +%FT%TZ) DOWN $url"
    /opt/shop/scripts/alert.sh "$url is down" || true
  fi
done
