#!/usr/bin/env bash
# Wildcard certificate for acowork-relay (design doc 24).
#
# *.relay.example.com MUST use the DNS-01 challenge (HTTP-01 cannot
# issue wildcards). Replace `dns-cloudflare` with your provider's
# certbot plugin (dns-route53, dns-he, dns_ali via a manual hook, ...).
#
# First issuance:
set -euo pipefail

DOMAIN=relay.example.com

# 1) Provider API credentials (never world-readable)
sudo mkdir -p /etc/letsencrypt/api-credentials
sudo tee /etc/letsencrypt/api-credentials/cloudflare.ini >/dev/null <<'INI'
dns_cloudflare_api_token = <your-token>
INI
sudo chmod 600 /etc/letsencrypt/api-credentials/cloudflare.ini

# 2) Issue cert + wildcard
sudo certbot certonly --dns-cloudflare \
  --dns-cloudflare-credentials /etc/letsencrypt/api-credentials/cloudflare.ini \
  -d "$DOMAIN" -d "*.$DOMAIN"

# 3) Reload the relay so it picks up the new key material
sudo systemctl restart acowork-relay

# Auto-renewal: certbot's own systemd timer handles renew, but the relay
# does NOT hot-swap certificates — a restart must happen on every
# successful renewal. Do NOT use `--deploy-hook`: certbot's timer is a
# separate process that does not carry your CLI flags, so the hook would
# silently never run. Drop an executable into the hooks directory
# instead (picked up by both the timer and a manual `certbot renew`):
#
#   sudo install -d /etc/letsencrypt/renewal-hooks/deploy
#   echo 'systemctl restart acowork-relay' | \
#     sudo tee /etc/letsencrypt/renewal-hooks/deploy/reload-relay.sh
#   sudo chmod +x /etc/letsencrypt/renewal-hooks/deploy/reload-relay.sh
#
# Then verify the whole chain: sudo certbot renew --dry-run
# (the dry run fires deploy hooks), and check the timer is armed:
#   systemctl list-timers | grep certbot
