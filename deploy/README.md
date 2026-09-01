# Deployment

First-time setup on a fresh Ubuntu / Debian VPS, then ongoing
deployments via `deploy.sh`.

## Prerequisites on the VPS

- nginx installed and running
- certbot installed (`apt install certbot python3-certbot-nginx`)
- A-record `smrt.hivens.dev` pointing at the VPS IP, propagated

## One-time VPS bootstrap

Run as root on the VPS:

```bash
# 1. service user + storage dir
useradd -r -s /usr/sbin/nologin -d /var/lib/smrt -M smrt
mkdir -p /var/lib/smrt/{packs,servers,cache}
chown -R smrt:smrt /var/lib/smrt

# 2. config dir + env file
mkdir -p /etc/smrt
cat > /etc/smrt/env <<EOF
SMRT_BIND_ADDR=127.0.0.1:9000
SMRT_STORAGE_DIR=/var/lib/smrt
# The origin baked into every manifest's source URLs. Set it before the first
# build: a manifest is frozen, so one built under the default points every
# launcher that downloads it at 127.0.0.1 forever.
SMRT_MIRROR_BASE=https://smrt.hivens.dev
# Machine auth for the CLI and scripts. Not a human login -- the panel's token
# form is gone, and a valid token there answers 410.
SMRT_ADMIN_TOKEN=$(openssl rand -base64 32)
# Panel sign-in. Without a GitHub OAuth app and at least one uid on the
# allowlist there is no way into the operator panel at all.
SMRT_GITHUB_CLIENT_ID=<from the GitHub OAuth app>
SMRT_GITHUB_CLIENT_SECRET=<from the GitHub OAuth app>
SMRT_ADMIN_GITHUB_UIDS=<your numeric github uid>
# Who owns operator-authored packs, and the backfill for packs predating the
# field. Same uid as above on a single-operator mirror.
SMRT_OPERATOR_UID=<your numeric github uid>
RUST_LOG=smrt=info,tower_http=info
EOF
chmod 640 /etc/smrt/env
chown root:smrt /etc/smrt/env

# 3. systemd unit (replace path with where you cloned this repo)
cp deploy/smrt.service /etc/systemd/system/smrt.service
systemctl daemon-reload

# 4. nginx site (HTTP-only initially; certbot fills HTTPS)
cp deploy/smrt.nginx.conf /etc/nginx/sites-available/smrt.conf
ln -s /etc/nginx/sites-available/smrt.conf /etc/nginx/sites-enabled/smrt.conf
nginx -t && systemctl reload nginx

# 5. issue cert (certbot edits the 443 block in-place)
certbot --nginx -d smrt.hivens.dev --non-interactive --agree-tos --email admin@hivens.dev

# 6. push the binary (from your dev machine, see Ongoing deployment below)

# 7. enable + start once the binary is in place
systemctl enable --now smrt
systemctl status smrt
```

## Ongoing deployment

From your dev machine:

```bash
./deploy/deploy.sh
```

The script builds `--release`, scp's **both** binaries to
`/usr/local/bin/<name>.new`, atomically swaps them into place, and restarts the
systemd unit. Both go together on purpose: `smrt-pack` opens the same
`registry.db` the service migrates at start, so shipping one without the other
leaves an on-box CLI that cannot read its own database. Override host, key or
target directory via env:

```bash
HOST=root@hivens.dev KEY=~/.ssh/other_key REMOTE_DIR=/usr/local/bin ./deploy/deploy.sh
```

## Verification

```bash
curl -s https://smrt.hivens.dev/v1/health | jq
```

Expected -- `version` is `<year>.<commit height>`, so it moves with every
deploy; what matters is that it changed and that `status` is `ok`:

```json
{"schema_version":2,"status":"ok","version":"2026.388"}
```

## Logs

```bash
ssh root@hivens.dev journalctl -u smrt -n 100 -f
```

## Rotating the admin token

```bash
# on VPS
sed -i "s|^SMRT_ADMIN_TOKEN=.*|SMRT_ADMIN_TOKEN=$(openssl rand -base64 32)|" /etc/smrt/env
systemctl restart smrt
```
