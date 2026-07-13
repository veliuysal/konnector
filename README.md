# Konnector

High-performance reverse proxy for Linux and Windows.

## Install on Ubuntu (automatic from GitHub)

```sh
curl -fsSL https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.sh | sudo bash
```

Install a specific version:

```sh
KONNECTOR_VERSION=v0.1.0 curl -fsSL https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.sh | sudo bash
```

Configs: `/opt/konnector/current/configs/`  
Env file: `/etc/konnector.env`  
Logs: `/etc/konnector/logs/` (`main/`, per-yaml folders, `watchers/`)

## Install on Windows (Administrator PowerShell)

```powershell
irm https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.ps1 | iex
```

Install a specific version:

```powershell
$env:KONNECTOR_VERSION='v0.1.0'; irm https://raw.githubusercontent.com/veliuysal/konnector/main/scripts/install.ps1 | iex
```

Or download the release zip and install:

```powershell
# From an elevated PowerShell, after extracting the zip:
.\konnector.exe install .\konnector-v0.1.0-windows-x86_64.zip
konnector start
konnector status
```

Install paths:

- Runtime: `C:\Program Files\Konnector\`
- Env file: `C:\ProgramData\Konnector\konnector.env`
- Configs: `C:\ProgramData\Konnector\configs\`
- Logs: `C:\ProgramData\Konnector\logs\` (`main\`, per-yaml folders, `watchers\`)
- Windows service name: `Konnector`

Ports 80/443 require an elevated process or the Windows service.

## Update

Linux:

```sh
sudo konnector update
sudo konnector install v0.1.0
```

Windows (elevated):

```powershell
konnector update
konnector install v0.1.0
```

## Configure a site

### Linux

```sh
sudo mkdir -p /etc/konnector/configs
sudo cp /opt/konnector/current/configs/example.yaml /etc/konnector/configs/mysite.yaml
sudo nano /etc/konnector/configs/mysite.yaml
```

In `mysite.yaml` set `enabled: true`, your upstream, and the hosts this site should answer:

```yaml
enabled: true
domains:
  - myapp.com
  - www.myapp.com
  - "*.myapp.com"    # optional: any one-level subdomain
proxy:
  mode: direct
  upstream:
    instance: 127.0.0.1:3000
```

Konnector already listens on HTTP/HTTPS (default `:80` / `:443`) for all sites; routing uses the request `Host`.  
For Let's Encrypt, list concrete names (wildcards need Cloudflare Origin CA or DNS-01).

Use **one YAML per site** (or group). Every `*.yaml` in `CONFIG_DIR` (except `root.yaml` and `*.tcp.yaml`) is loaded:

```text
configs/
  shop.yaml      # shop.com, www.shop.com, *.shop.com
  blog.yaml      # blog.example.org
  api.yaml       # api.myapp.com
  root.yaml      # fallback when Host matches no site
```

Exact hostnames win over wildcards when both could match.

### Listeners (http / https)

Sites default to **both** HTTP and HTTPS. Restrict with `listen` if needed:

```yaml
# listen: both   # default — required for Cloudflare Full/Strict to origin :443
listen: https    # TLS only (also: 443)
listen: http     # plain HTTP only (also: 80)
```

Same hostname can be split across two YAMLs (one `listen: http`, one `listen: https`).

Force HTTP → HTTPS with a 308 (keep path and query):

```yaml
listen: both
redirect_https: true
```

ACME HTTP-01 challenges are answered before this redirect.

### Cloudflare Origin CA (optional)

Behind orange-cloud with **Full (strict)**, set a token so Origin CA is used instead of Let's Encrypt:

```text
CLOUDFLARE_API_TOKEN=...   # Origin CA Create Certificate permission
```

No other TLS settings are required. On start Konnector fetches the Origin cert; the watcher renews when needed.

Site YAML tip: `forwarding: cloudflare` trusts CF headers.

### WebSocket (ws / wss)

Sites default to **regular HTTP traffic only**. Enable WebSocket with `traffic`:

```yaml
# Same upstream for HTTP + WebSocket:
traffic: both

# Or WebSocket-only site (can share a domain with an http-only YAML):
traffic: websocket
```

```yaml
enabled: true
domains:
  - app.example.com
listen: both
traffic: both
proxy:
  mode: load_balanced
  upstreams:
    - instance: 127.0.0.1:8080
    - instance: 127.0.0.1:8081
  health_check: true
```

Clients connect with `ws://` or `wss://` (TLS terminates on Konnector when HTTPS is enabled). Logs include `websocket=true`.

Set in `/etc/konnector.env`:

```text
CONFIG_DIR=/etc/konnector/configs
```

### Windows

```powershell
New-Item -ItemType Directory -Force -Path 'C:\ProgramData\Konnector\configs'
Copy-Item 'C:\Program Files\Konnector\current\configs\example.yaml' 'C:\ProgramData\Konnector\configs\mysite.yaml'
notepad 'C:\ProgramData\Konnector\configs\mysite.yaml'
```

`CONFIG_DIR` is already set in `C:\ProgramData\Konnector\konnector.env` by default.

Restart:

```sh
# Linux
sudo konnector restart

# Windows (elevated)
konnector restart
```

## Automatic HTTPS

HTTPS is **on by default**. The server picks the certificate source automatically — you do not set a provider:

- `CLOUDFLARE_API_TOKEN` set → Cloudflare Origin CA  
- otherwise → Let's Encrypt (ACME)

Just enable a site with real domains and restart. Certs are issued/renewed into `TLS_DIR` (default `/etc/ssl/konnector`).

Optional:

```text
# TLS_ENABLED=false          # turn HTTPS off
# CLOUDFLARE_API_TOKEN=...   # prefer Origin CA (behind Cloudflare Full strict)
# ACME_STAGING=true          # Let's Encrypt staging
```

For Let's Encrypt, DNS must point here and port **80** must be reachable (HTTP-01). Wildcards need Cloudflare Origin CA.

Files under `TLS_DIR`:

- `fullchain.pem` / `privkey.pem` — served cert
- `acme/` — Let's Encrypt account data

Until a real cert is ready, Konnector serves a temporary self-signed cert so `:443` still listens.

## Logs

File logs (override with `LOGS_DIR`):

```text
/etc/konnector/logs/
  main/konnector.log          # process / general logs
  root/access.log             # root.yaml (when root proxy enabled)
  shop/access.log             # shop.yaml traffic
  blog/access.log             # blog.yaml traffic
  postgres.tcp/access.log     # *.tcp.yaml traffic
  watchers/config.log         # config watcher
  watchers/tls.log            # TLS watcher
```

Every **enabled** YAML gets its own folder as soon as it loads. Access lines go there automatically (default `logging.level: info`; set `off` to silence a site).

`konnector logs` shows `logs/main/konnector.log`.  
`konnector logs example` shows that YAML’s access log.  
`konnector logs watchers/config` shows the config watcher log.

## Commands

```sh
konnector status
konnector health
konnector restart
konnector logs --follow
konnector tags
konnector uninstall
```

## Development

```sh
# Linux (port 80 needs root)
sudo cargo run
curl http://127.0.0.1/_health

# Or use a high port
HTTP_LISTEN=127.0.0.1:8080 cargo run -- serve
```

```powershell
# Windows
$env:HTTP_LISTEN='127.0.0.1:8080'
cargo run -- serve
```

```sh
cargo test --locked
```
