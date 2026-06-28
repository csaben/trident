<p align="center">
  <img src="assets/trident channeling 2.jpg" alt="Trident Channeling" width="280">
</p>

<h1 align="center">trident</h1>

<p align="center">
  Forked Claude Code sessions that hand tasks to each other across machines -
  one session drops a task straight into another's chat, no copy-paste.
</p>

<p align="center">
  🔱 Named for Minecraft's <strong>Channeling</strong> enchant: strike one trident, the bolt lands elsewhere.<br>
  <a href="https://csaben.github.io/trident/"><strong>Website</strong></a> ·
  <a href="docs/README.md"><strong>Full guide & internals</strong></a>
</p>

---

## Prerequisites

- **Claude Code v2.1.80+** on each machine (channels are research-preview).
- **[Tailscale](https://tailscale.com)** on every machine - trident routes between them over your tailnet ([quickstart](docs/README.md#tailscale)).
- The binary is self-contained - no runtime to install.

## Install

```bash
# macOS / Linux / WSL
curl -fsSL https://raw.githubusercontent.com/csaben/trident/main/install.sh | sh
```
```powershell
# native Windows PowerShell
irm https://raw.githubusercontent.com/csaben/trident/main/install.ps1 | iex
```

## Run

```bash
trident host                       # this machine runs the hub + a session
trident join http://100.x.y.z:8790 # every other machine points at the hub
```

Each launches Claude Code with the channel and `--rc`, so every session is watchable from [claude.ai/code](https://claude.ai/code) and your phone. First run prompts to register the channel for all sessions and pick your skip-permissions default. Then just say *"have the backend session add a /tts endpoint"* and trident carries it over.

## Commands

| Command                                  | What it does                                                       |
| ---------------------------------------- | ------------------------------------------------------------------ |
| `trident host`                           | Become the hub, enlist tailnet peers, launch a session.            |
| `trident host --no-enlist \| --user U \| --dry-run` | Skip the picker · set SSH login for peers · print without running. |
| `trident join [hub]`                     | Point at a hub (persisted) and launch a session.                   |
| `trident join --yes \| --skip-perms on\|off \| --rc on\|off` | Non-interactive register · override skip-perms · override --rc.    |
| `trident config show`                    | Print settings + the exact `claude` command they resolve to.       |
| `trident config set-hub \| host \| set-name \| skip-perms \| rc` | Re-point or re-default without reinstalling.        |

## Sample

```text
$ trident host
hub address for peers:  http://100.111.42.8:8790

Online tailnet peers:
  [1] cluster (100.a.*)   linux
  [2] webserver (100.b.*)  linux
Enlist which? (e.g. 1,3  or  all  - blank to skip): 1
→ enlisting cluster (100.a.*)...
  ✓ cluster enlisted (watch it at claude.ai/code)
trident: launching claude --dangerously-load-development-channels server:trident --rc
```

Everything else - architecture, config, the WSL Tailscale gotcha, security model, and building from source - is in **[docs/README.md](docs/README.md)**.

## License

MIT
