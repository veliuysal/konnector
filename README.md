# Konnector

Reverse proxy built on Cloudflare Pingora.

## Install on Ubuntu server (v0.1.0)

Run on the server as root:

```sh
sudo apt update
sudo apt install -y curl ca-certificates

curl -fsSL -o /tmp/konnector.deb \
  https://github.com/veliuysal/konnector/releases/download/v0.1.0/konnector_0.1.0-1_amd64.deb

sudo apt install -y /tmp/konnector.deb
```

Check:

```sh
konnector status
konnector health
```

Configs are at `/opt/konnector/current/configs/`.  
Env file: `/etc/konnector.env`

## After a newer release is published

```sh
sudo konnector update
# or
sudo konnector install v0.2.0
```

## Configure a site

```sh
sudo cp /opt/konnector/current/configs/example.yaml /etc/konnector/configs/mysite.yaml
sudo mkdir -p /etc/konnector/configs
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

```sh
sudo konnector start
sudo konnector stop
sudo konnector restart
konnector status
konnector health
konnector logs --follow
konnector tags
```

## Development

```sh
cargo run
curl http://127.0.0.1/_health
cargo test --locked
```
