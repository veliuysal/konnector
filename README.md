# Konnector

High-performance reverse proxy.

## Install on Ubuntu server (automatic from GitHub)

Installs the `.deb` package from the latest GitHub release.

One command:

```sh
curl -fsSL https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.sh | sudo bash
```

Install a specific version:

```sh
KONNECTOR_VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.sh | sudo bash
```

Or install the `.deb` manually:

```sh
curl -fsSL -o /tmp/konnector.deb https://github.com/veliuysal/konnector/releases/download/v0.1.0/konnector_0.1.0-1_amd64.deb
sudo apt install -y /tmp/konnector.deb
```

Check:

```sh
konnector status
konnector health
```

Configs: `/opt/konnector/current/configs/`  
Env file: `/etc/konnector.env`

## Update

Updates via the `.deb` package from GitHub (CLI + proxy server):

```sh
sudo konnector update
```

Or install a specific release:

```sh
sudo konnector install v0.1.0
```

Or update with curl only:

```sh
DEB_URL=$(curl -fsSL -H "Accept: application/vnd.github+json" \
  https://api.github.com/repos/veliuysal/konnector/releases/latest \
  | sed -n 's/.*"browser_download_url": "\([^"]*_amd64\.deb\)".*/\1/p' | head -1)

curl -fsSL -o /tmp/konnector.deb "$DEB_URL"
sudo apt install -y /tmp/konnector.deb
```

Your site configs are kept during update. Prefer `/etc/konnector/configs` with `CONFIG_DIR` set in `/etc/konnector.env`.

## Configure a site

```sh
sudo mkdir -p /etc/konnector/configs
sudo cp /opt/konnector/current/configs/example.yaml /etc/konnector/configs/mysite.yaml
sudo nano /etc/konnector/configs/mysite.yaml
```

Set in `/etc/konnector.env`:

```text
CONFIG_DIR=/etc/konnector/configs
```

Restart:

```sh
sudo konnector restart
```

## Commands

Use subcommands. Do not run bare `konnector` — the service already owns port 80.

```sh
konnector status
konnector health
sudo konnector restart
konnector logs --follow
konnector tags
sudo konnector uninstall
```

## Development

```sh
sudo cargo run
curl http://127.0.0.1/_health
```

`cargo run` starts the proxy on `0.0.0.0:80` using `configs/` from the repo.
Port 80 requires root locally, same as production.

```sh
cargo test --locked
```
