# Konnector

Reverse proxy built on Cloudflare Pingora. Download the release binary, run
`konnector install`, add site YAML files, and manage the service with the same
command.

## Quick start

Download the latest release tarball, extract the binary, and install:

```sh
curl -fsSL -o /tmp/konnector.tgz \
  "$(curl -fsSL -H 'Accept: application/vnd.github+json' \
    https://api.github.com/repos/veliuysal/konnector/releases/latest \
    | jq -r '.assets[] | select(.name | test("konnector-v.*\\.tar\\.gz$")) | .browser_download_url')"
tar -xzf /tmp/konnector.tgz -C /tmp konnector
chmod +x /tmp/konnector
sudo /tmp/konnector install
```

Set `KONNECTOR_GITHUB_REPO` to override the default repository
(`veliuysal/konnector` or `https://github.com/veliuysal/konnector.git`).

```sh
konnector status
konnector health
```

Run without arguments to start the proxy server.

## Configuration

Site configs live in YAML files under `CONFIG_DIR` (default: `configs` next to the
runtime binary). Each file defines one or more domains and one upstream target.

```yaml
domains:
  - example.com

proxy:
  mode: direct
  upstream:
    instance: 127.0.0.1:8080

redirects:
  - from: /home
    to: /
    behavior: redirect
    match: exact

forwarding: direct
```

The repository ships `configs/example.yaml` as a starting point. Replace it with
your own site files in production.

Environment settings are read from `/etc/konnector.env`:

```text
HTTP_LISTEN=0.0.0.0:80
HTTPS_LISTEN=0.0.0.0:443
THREADS=4
CONFIG_DIR=/etc/konnector/configs
```

Optional root fallback when no site matches is configured in `configs/root.yaml`:

```yaml
upstream:
  instance: 127.0.0.1:3000
```

`ROOT_PROXY` in `/etc/konnector.env` overrides `root.yaml` when set.

## Commands

```sh
sudo konnector install
sudo konnector install v0.1.0
sudo konnector install --tag v0.1.0
sudo konnector install ./konnector-v0.1.0.tar.gz
konnector tags
sudo konnector update
sudo konnector update v0.1.0
sudo konnector start
sudo konnector stop
sudo konnector restart
konnector releases
konnector current
konnector logs --follow
konnector help
```

`install` fetches the latest GitHub release when no argument is given. Pass a tag
(`v0.1.0` or `0.1.0`), archive path, or release URL to install a specific version.
Use `konnector tags` to list published release tags.

It creates the `konnector` system user, installs the systemd unit, writes
`/etc/konnector.env`, extracts the release under `/opt/konnector/releases`, links
`/opt/konnector/current`, and starts the service.

Build a Debian package from source:

```sh
sudo konnector build-deb
```

## Releases

Publish a GitHub release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow uploads:

- `konnector_<version>_amd64.deb`
- `konnector-v<tag>.tar.gz` (binary + bundled `configs/`)

Update an installed server:

```sh
sudo konnector update
```

## HTTPS

Enable TLS in `/etc/konnector.env`:

```text
TLS_ENABLED=true
TLS_CERT_PATH=/etc/ssl/konnector/fullchain.pem
TLS_KEY_PATH=/etc/ssl/konnector/privkey.pem
```

Optional automatic refresh:

```text
TLS_PROVIDER=cloudflare
CLOUDFLARE_API_TOKEN=...
```

## Development

```sh
cargo run
curl http://127.0.0.1/_health
```

Tests:

```sh
cargo test --locked
cargo clippy --all-targets -- -D warnings
```
