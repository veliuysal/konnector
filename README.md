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
- Logs: `C:\ProgramData\Konnector\logs\konnector.log`
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
