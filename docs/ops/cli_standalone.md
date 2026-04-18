# Standalone CLI

`tv-bot-cli` is the standalone operator surface for the local runtime host. It talks only to the local control API.

## What It Covers

- launch the runtime
- inspect status, readiness, and history
- load a strategy
- start or mark warmup
- switch mode
- arm and disarm
- pause and resume
- resolve reconnect review
- resolve shutdown review
- flatten a contract through the audited execution path

## Basic Usage

```powershell
.\target\release\tv-bot-cli.exe --help
```

Global options:

- `--config <path>`
- `--base-url <url>`

If `--base-url` is omitted, the CLI uses the runtime host bind from config.

## Command Surface

- `launch`
- `status`
- `readiness`
- `history`
- `load <path>`
- `warmup start|ready|fail`
- `start <paper|live|observation>`
- `pause`
- `resume`
- `arm`
- `disarm`
- `reconnect-review <close-position|leave-broker-protected|reattach-bot-management>`
- `shutdown <flatten-first|leave-broker-protected>`
- `flatten <contract-id>`

## Common Flows

### Launch The Runtime

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml launch
```

Optional:

- `--runtime-bin <path>`
- `--strategy <path>`

### Inspect Runtime State

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml status
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml readiness
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml history
```

### Load A Strategy

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml load .\strategies\examples\micro_silver_elephant_tradovate_v1.md
```

### Warmup Flow

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml warmup start
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml readiness
```

If you are simulating or forcing a state during tests:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml warmup ready
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml warmup fail --reason "provider unavailable"
```

### Mode And Arming

Observation:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml start observation
```

Paper:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml start paper
```

Live:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml start live
```

Arm:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml arm
```

Arm with override:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml arm --allow-override
```

Disarm:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml disarm
```

## Safety Review Commands

### Reconnect Review

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml reconnect-review close-position --contract-id 12345 --reason "operator chose close"
```

Other decisions:

- `leave-broker-protected`
- `reattach-bot-management`

### Shutdown Review

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml shutdown flatten-first --contract-id 12345 --reason "operator approved shutdown"
```

Alternative:

- `leave-broker-protected`

### Flatten

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml flatten 12345 --reason "manual flatten"
```

## Confirmations

Dangerous commands prompt for confirmation unless `--yes` is supplied.

Example:

```powershell
.\target\release\tv-bot-cli.exe --config .\config\runtime.local.toml arm --yes
```

## What To Check During Debugging

- `status` for mode, arm, account, broker, market data, storage, journal, and dispatch availability
- `readiness` for the full check list and override requirements
- `history` for recent runs, orders, fills, positions, trades, and aggregate PnL

For deeper debugging, pair the CLI with:

- `docs/ops/debugging_guide.md`
- `docs/ops/credential_setup.md`
