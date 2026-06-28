# trident - full guide

Internals, configuration, and the cross-machine setup details. For the quick start see the [root README](../README.md); for the overview see the [website](https://csaben.github.io/trident/).

## How it works

trident is a Claude Code **channel** - an MCP server each session runs locally. Every session connects to one shared **hub** that routes messages between them, so any session can inject a task into any other.

```
 machine A (desktop)              machine B (laptop)
 ┌──────────────┐                ┌──────────────┐
 │ Claude Code  │   "add /tts"   │ Claude Code  │
 │   trident    │ ─────────────► │   trident     │
 └──────┬───────┘                └──────┬───────┘
        │            hub                │
        └───────────► ◄─────────────────┘
            routes between sessions
```

1. **Each session runs the trident channel** - a small server Claude Code spawns over stdio. It exposes two tools: `trident_roster` (who's online) and `trident_send` (hand a task to a session by name, or broadcast to all).
2. **A hub routes between them.** One lightweight broker every session dials into. The first machine starts it automatically; everyone else points at it over Tailscale by its stable `100.x` IP.
3. **Messages arrive as native chat.** A sent task lands in the target session as a `<channel source="trident" from="...">` event, so that Claude reads it as a hand-off and acts on it in its own folder.
4. **Watch from anywhere.** Sessions launch with Claude Code's `--rc` (remote control), so they appear in [claude.ai/code](https://claude.ai/code) and the mobile apps, synced.

## Prerequisites

- **Claude Code v2.1.80+** on each machine (channels are a research-preview feature).
- **[Tailscale](https://tailscale.com)** on every machine that joins.
- The trident binary is self-contained - no runtime to install.

## Tailscale

trident routes between machines over your tailnet, so every machine must be on it. If you're new to Tailscale, it's a zero-config WireGuard mesh - install, log in with the same account on each machine, and they reach each other by a stable `100.x` IP from anywhere.

1. **Install** on each machine: <https://tailscale.com/download> (or `curl -fsSL https://tailscale.com/install.sh | sh` on Linux).
2. **Join the tailnet** - use the **same account or org** everywhere:
   ```bash
   sudo tailscale up
   # on the machine that will orchestrate the fleet, enable SSH:
   sudo tailscale up --ssh
   ```
3. **Find a machine's tailnet IP** (the hub address):
   ```bash
   tailscale ip -4        # e.g. 100.111.42.8
   ```
4. **Verify peers see each other:**
   ```bash
   tailscale status
   ```

No firewall changes are needed for Tailscale - traffic rides the encrypted tailnet interface.

## Install

```bash
# macOS / Linux / WSL / Git Bash
curl -fsSL https://raw.githubusercontent.com/csaben/trident/main/install.sh | sh
```
```powershell
# native Windows PowerShell
irm https://raw.githubusercontent.com/csaben/trident/main/install.ps1 | iex
```

The installer detects your OS/arch and pulls the matching release binary. To build from source instead: `cargo install --git https://github.com/csaben/trident`.

## Configuring without reinstalling

Everything lives in `~/.config/trident/config.toml`; change it any time and restart the session - no reinstall:

| Command                                   | Effect                                                            |
| ----------------------------------------- | ---------------------------------------------------------------- |
| `trident config show`                     | Print current settings + the exact `claude` command they resolve to. |
| `trident config set-hub http://IP:8790`   | Point this machine at a different hub.                            |
| `trident config host`                     | This machine is the hub (sessions use localhost; broker binds `0.0.0.0`). |
| `trident config set-name <name>`          | Set this session's roster name.                                  |
| `trident config skip-perms on\|off`       | Default `--dangerously-skip-permissions` for launched sessions.  |
| `trident config rc on\|off`               | Default `--rc` (remote control) for launched sessions.           |

Env vars (`TRIDENT_HUB`, `TRIDENT_HUB_PORT`, `TRIDENT_NAME`) override the file for one-off runs. To unregister, remove the `trident` block from `~/.claude.json`.

## Launch the whole fleet from one machine

`trident host` starts the hub, then lists your online Tailscale peers and lets you enlist them:

```
Online tailnet peers:
  [1] cluster (100.a.*)   linux
  [2] webserver (100.b.*)  linux
Enlist which? (e.g. 1,3  or  all  - blank to skip):
```

For each peer you pick, trident SSHes in (Tailscale SSH, falling back to `ssh`), installs itself if missing, and starts a `trident join` session inside `tmux` pointed at this hub - autonomous (`--skip-perms`) and remote-controlled (`--rc`). One command, whole fleet.

| `trident host` flag | Effect                                                              |
| ------------------- | ------------------------------------------------------------------ |
| `--no-enlist`       | Just be the hub + launch locally; skip the picker.                 |
| `--user <u>`        | SSH login to use for enlisted peers (default: tailnet/current user). |
| `--dry-run`         | Print the SSH bootstrap script for each peer without running it.    |

The peer and folder pickers support typed filtering and scrolling (arrow keys). The SSH username and chosen folder are remembered per peer (`[peers.<name>]` in config), so after the first enlist you can Enter through the prompts.

`trident join` accepts `--yes` (non-interactive registration), `--skip-perms on|off`, and `--rc on|off` for scripted/remote use. Each enlisted peer needs `tmux` and Claude Code installed, plus an SSH path you can reach.

### Driving from inside a session

Run **`trident`** with no subcommand to become the hub and launch a normal Claude Code session - no peer picker - so you can remote-spawn later without committing to a fleet up front. From inside that session, two auto-installed slash commands drive the fleet:

- **`/trident-new [peer]`** - spawn a worker session on a tailnet peer (calls `trident enlist` under the hood).
- **`/trident-peers`** - list online peers and the connected roster.

They're plain prompt files in `~/.claude/commands/` (installed on first `trident`/`trident host`; reinstall with `trident install-commands`). Underneath, two non-interactive commands are also usable directly or in scripts:

- `trident peers` - print online tailnet peers as `name<TAB>ip<TAB>os`.
- `trident enlist <peer> [--dir <path>] [--user <login>]` - spawn one peer against the local hub.

**How the SSH connection is made.** trident tries **Tailscale SSH** first (no keys, no host-key prompts - turnkey if you ran `tailscale up --ssh` on the peer). If the peer doesn't have Tailscale SSH enabled, it falls back to plain `ssh`:

- It accepts and saves the peer's host key on first connect (`StrictHostKeyChecking=accept-new`), so the "No host key is known ... strict checking" error doesn't block you.
- You're asked for the login **once**. Set the username with `trident host --user <name>` or at the prompt; the password is entered through ssh.
- On that first connect it **installs your public key** on the peer (generating `~/.ssh/id_ed25519` if you don't have one), so every later enlist is passwordless.

## Same LAN instead of Tailscale

Works the same on a plain LAN - use the host's LAN IP (`192.168.x.x`) instead of the Tailscale IP, and open the port on the hub host once:

```powershell
# Windows (PowerShell, on the hub host):
New-NetFirewallRule -DisplayName trident-hub -Direction Inbound -Protocol TCP -LocalPort 8790 -Action Allow
```

Both machines must reach the hub host's `:8790`; check with `curl http://<hub-ip>:8790/roster` before launching (a JSON roster means you're good).

## Running the host inside WSL (Tailscale gotcha)

If you run `claude` inside **WSL** on the hub host, use **WSL's** Tailscale IP, not the Windows host's. WSL2 is a separate tailnet device with its own `100.x` address, and the hub binds inside WSL's network namespace - the Windows host's Tailscale IP won't route to it.

```bash
# inside WSL on the hub host:
tailscale ip -4        # e.g. 100.118.7.33   <-- use THIS as the hub IP
```
```bash
# on every other machine:
trident join http://100.118.7.33:8790
```

Symptom of getting this wrong: the remote's `/mcp` shows `trident · connected · 2 tools` (it reached *its own* Claude fine), but `trident_roster` on the host never lists it - its hub URL points at an address with nothing on `:8790`. Same if you forget the `:8790` port (it silently becomes port 80). Sanity-check with `curl http://<hub-ip>:8790/roster` from the remote. WSL's IP can change across reboots; use the node's MagicDNS name (`http://<wsl-hostname>:8790`) for stability.

## Security / trust boundary

Anyone who can reach the hub port can inject text into your sessions - and with `--skip-perms` enabled, that text can run commands. Keep the hub on your tailnet (or a firewalled LAN port); **don't expose `:8790` to the open internet.** Inbound messages are tagged with the sender session's name. Skip-permissions is a deliberate, toggleable default (`trident config skip-perms off`), not silent.

## Building / hacking

The binary is a small Rust crate ([`src/`](../src/)) with subcommands: `serve-mcp` (the stdio MCP channel server Claude Code spawns), `hub` (the broker), and the `join` / `host` / `config` / `enlist` launcher. `cargo build --release` produces a ~1.2 MB static binary; releases for Linux/macOS/Windows are built by [`.github/workflows/release.yml`](../.github/workflows/release.yml) on every `v*` tag.

Built for the channels **research preview**; needs Claude Code v2.1.80+ and the `--dangerously-load-development-channels` flag. Packaging as an installable plugin is a later step.
