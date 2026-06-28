// The user-facing control plane: `trident config`, `trident join`,
// `trident host`. These manage ~/.config/trident/config.toml, register the
// channel in Claude Code, launch `claude`, and (for `host`) enlist tailnet
// peers over SSH so the whole fleet comes up from one machine.

use std::io::{stdin, stdout, IsTerminal, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{self, Config};

const INSTALL_URL: &str = "https://raw.githubusercontent.com/csaben/trident/main/install.sh";

// Slash-command templates installed into ~/.claude/commands. Intentionally
// generic - no machine names, IPs, usernames, or paths.
const CMD_NEW: &str = r#"---
description: Spawn a trident worker session on a remote tailnet machine
allowed-tools: Bash(trident:*)
---
You orchestrate "trident" - sibling Claude Code sessions across machines on a Tailscale network. This machine is the hub.

The user wants to spawn a trident worker session on a remote peer. Arguments (optional): $ARGUMENTS

Steps:
1. Run `trident peers` to list online tailnet peers (columns: name, ip, os).
2. Pick the target peer from the arguments if given; otherwise ask the user which peer.
3. If the user named a project/folder, pass it; otherwise omit it and the session starts in the peer's home directory.
4. Run `trident enlist <peer> [--dir <path>] [--user <login>]`. It spawns the session over SSH inside tmux with remote control enabled, pointed at this hub.
5. Report the outcome. Tell the user they can watch it at https://claude.ai/code or attach with `ssh -t <login>@<peer-ip> tmux attach -t trident`.

After it joins, hand it work with the trident_send tool (target = the peer name).
"#;

const CMD_PEERS: &str = r#"---
description: Show trident sessions and online tailnet peers
allowed-tools: Bash(trident:*)
---
Run `trident peers` to list online Tailscale peers (name, ip, os), and call the trident_roster tool to show which sessions are currently connected to the hub. Summarize both for the user.
"#;

// --- `trident config` ------------------------------------------------------

pub fn config_show() {
    let c = config::load();
    println!("config file: {}", config::config_path().display());
    println!("  hub:        {}", c.hub.clone().unwrap_or_else(|| "(local - this machine is the hub)".into()));
    println!("  name:       {}", c.name.clone().unwrap_or_else(|| "(random per session)".into()));
    println!("  skip_perms: {}", c.skip_perms);
    println!("  rc:         {}", c.rc);
    println!("  registered: {}", if is_registered() { "yes (~/.claude.json)" } else { "no" });
    println!("\nresolved at launch:");
    println!("  hub_url:    {}", config::hub_url());
    println!("  claude:     claude {}", launch_args(&c, &[], None, None).join(" "));
}

pub fn config_set_hub(url: String) -> anyhow::Result<()> {
    let mut c = config::load();
    c.hub = Some(url.trim_end_matches('/').to_string());
    config::save(&c)?;
    println!("hub set to {}", c.hub.unwrap());
    Ok(())
}

pub fn config_host_mode() -> anyhow::Result<()> {
    let mut c = config::load();
    c.hub = None;
    config::save(&c)?;
    println!("this machine is now the hub (sessions connect to localhost; the broker binds 0.0.0.0)");
    print_tailnet_hint();
    Ok(())
}

pub fn config_set_name(name: String) -> anyhow::Result<()> {
    let mut c = config::load();
    c.name = Some(name.clone());
    config::save(&c)?;
    println!("name set to {name}");
    Ok(())
}

pub fn config_set_skip_perms(on: bool) -> anyhow::Result<()> {
    let mut c = config::load();
    c.skip_perms = on;
    config::save(&c)?;
    println!("skip_perms = {on}");
    Ok(())
}

pub fn config_set_rc(on: bool) -> anyhow::Result<()> {
    let mut c = config::load();
    c.rc = on;
    config::save(&c)?;
    println!("rc = {on}");
    Ok(())
}

// --- `trident join [hub]` --------------------------------------------------

pub fn join(
    hub: Option<String>,
    name: Option<String>,
    assume_yes: bool,
    skip_perms: Option<bool>,
    rc: Option<bool>,
    dry_run: bool,
    claude_args: Vec<String>,
) -> anyhow::Result<()> {
    if let Some(h) = hub {
        config_set_hub(h)?;
    }
    if let Some(n) = name {
        config_set_name(n)?;
    }
    ensure_registered(assume_yes)?;
    let c = config::load();
    launch_claude(&c, &claude_args, skip_perms, rc, dry_run)
}

// --- `trident host` --------------------------------------------------------

/// `trident peers`: print online tailnet peers as `name<TAB>ip<TAB>os`, one per
/// line (parser-friendly for the slash commands).
pub fn peers() {
    let list = online_peers();
    if list.is_empty() {
        println!("(no online tailnet peers)");
        return;
    }
    for p in list {
        println!("{}\t{}\t{}", p.name, p.ip, p.os);
    }
}

/// `trident enlist <peer>`: non-interactive single-peer spawn against the local
/// hub (what the /trident-new slash command calls). Uses saved user/dir defaults.
pub fn enlist_one(peer: String, dir: Option<String>, user: Option<String>) -> anyhow::Result<()> {
    let port = config::hub_port();
    let Some(ip) = tailscale_ip() else {
        anyhow::bail!("no Tailscale IP found - can't reach peers");
    };
    ensure_hub(port);

    let online = online_peers();
    let Some(p) = online.iter().find(|p| p.name == peer || p.ip == peer) else {
        anyhow::bail!("'{peer}' is not an online tailnet peer (see `trident peers`)");
    };

    let ssh_user = user.or_else(|| config::peer(&p.name).user);
    let dir = dir.or_else(|| config::peer(&p.name).last_dir);
    let target = match &ssh_user {
        Some(u) => format!("{u}@{}", p.ip),
        None => p.ip.clone(),
    };

    let mut pc = config::peer(&p.name);
    pc.user = ssh_user.clone();
    if let Some(d) = &dir {
        pc.last_dir = Some(d.clone());
    }
    let _ = config::set_peer(&p.name, pc);

    let hub_url = format!("http://{ip}:{port}");
    let script = remote_script(&hub_url, &p.name, dir.as_deref());
    println!("→ enlisting {} ({})...", p.name, p.ip);
    match ssh_run(&target, &script) {
        Ok(s) if s.success() => {
            println!("✓ {} enlisted - watch at claude.ai/code, or: ssh -t {target} tmux attach -t trident", p.name);
            Ok(())
        }
        Ok(s) => anyhow::bail!("{} failed (exit {:?})", p.name, s.code()),
        Err(e) => anyhow::bail!("{} ssh error: {e}", p.name),
    }
}

/// Write the trident slash commands into ~/.claude/commands (idempotent; only
/// overwrites when `force`). Called on the orchestrator (start/host), not peers.
pub fn install_commands(force: bool) {
    let dir = config::home().join(".claude").join("commands");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    for (name, body) in [("trident-new.md", CMD_NEW), ("trident-peers.md", CMD_PEERS)] {
        let path = dir.join(name);
        if force || !path.exists() {
            let _ = std::fs::write(&path, body);
        }
    }
    if force {
        println!("Installed /trident-new and /trident-peers into {}", dir.display());
    }
}

/// Bare `trident`: become the hub and launch a normal Claude Code session -
/// no peer picker. You're set up to remote-spawn later without committing to a
/// fleet up front.
pub fn start(claude_args: Vec<String>) -> anyhow::Result<()> {
    {
        let mut c = config::load();
        if c.hub.is_some() {
            c.hub = None;
            config::save(&c)?;
        }
    }
    ensure_registered(false)?;
    install_commands(false);
    let port = config::hub_port();
    ensure_hub(port);
    match tailscale_ip() {
        Some(ip) => println!("trident: you are the hub - peers join with `trident join http://{ip}:{port}`"),
        None => println!("trident: hub running on localhost:{port} (install Tailscale to reach peers)"),
    }
    let c = config::load();
    launch_claude(&c, &claude_args, None, None, false)
}

pub fn host(
    dry_run: bool,
    no_enlist: bool,
    user: Option<String>,
    claude_args: Vec<String>,
) -> anyhow::Result<()> {
    {
        let mut c = config::load();
        if c.hub.is_some() {
            c.hub = None;
            config::save(&c)?;
        }
    }
    ensure_registered(false)?;
    install_commands(false);

    let port = config::hub_port();
    let Some(ip) = tailscale_ip() else {
        eprintln!("trident: no Tailscale IP found - can't orchestrate the fleet (install/up Tailscale first).");
        let c = config::load();
        return launch_claude(&c, &claude_args, None, None, dry_run);
    };

    let url = format!("http://{ip}:{port}");
    println!("hub address for peers:  {url}");

    // Own the hub explicitly and confirm peers can actually reach it BEFORE
    // enlisting - otherwise we'd hand peers a dead address (the classic
    // "enlisted but never joins the roster" failure).
    let reachable = if dry_run {
        true
    } else {
        ensure_hub(port);
        let ok = probe(&ip, port);
        if ok {
            println!("✓ peers can reach the hub here");
        } else {
            warn_unreachable(&ip, port);
        }
        ok
    };

    if !no_enlist {
        let go = reachable
            || prompt_yes_no("Enlist peers anyway? (they likely won't be able to connect)", false);
        if go {
            enlist(&ip, port, user.as_deref(), dry_run);
        } else {
            println!("Skipping enlist - fix reachability above and re-run `trident host`.");
        }
    }

    let c = config::load();
    launch_claude(&c, &claude_args, None, None, dry_run)
}

/// Make sure a hub is listening locally, starting one if needed. The hub binds
/// 0.0.0.0, so once it's up it's reachable on the tailnet too (firewall permitting).
fn ensure_hub(port: u16) {
    if probe("127.0.0.1", port) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("hub")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(150));
        if probe("127.0.0.1", port) {
            return;
        }
    }
}

/// TCP-connect probe - true if something accepts a connection at host:port.
fn probe(host: &str, port: u16) -> bool {
    match format!("{host}:{port}").parse() {
        Ok(addr) => TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).is_ok(),
        Err(_) => false,
    }
}

fn warn_unreachable(ip: &str, port: u16) {
    eprintln!("\n⚠ Peers can't reach the hub at http://{ip}:{port} yet. Likely causes:");
    if cfg!(windows) {
        eprintln!("  • Windows Firewall is blocking inbound :{port}. In an ELEVATED PowerShell:");
        eprintln!("      Get-NetFirewallRule -DisplayName trident -EA SilentlyContinue | Remove-NetFirewallRule");
        eprintln!("      New-NetFirewallRule -DisplayName trident -Direction Inbound -Protocol TCP -LocalPort {port} -Action Allow");
    }
    eprintln!("  • A stale hub is holding port {port} (e.g. an old Claude session, or one in WSL).");
    eprintln!("    If you run Claude in WSL, host there and use WSL's Tailscale IP instead.");
    eprintln!("  Verify with:  curl http://{ip}:{port}/roster\n");
}

// --- fleet enlistment over SSH ---------------------------------------------

struct Peer {
    name: String,
    ip: String,
    os: String,
}

fn enlist(hub_ip: &str, port: u16, user: Option<&str>, dry_run: bool) {
    let peers = online_peers();
    if peers.is_empty() {
        println!("(no online Tailscale peers to enlist)");
        return;
    }

    let selected: Vec<usize> = if dry_run {
        (0..peers.len()).collect()
    } else if !stdin().is_terminal() {
        println!("(non-interactive: skipping enlist)");
        return;
    } else {
        // Typed-filter multi-select (space toggles, enter confirms).
        let opts: Vec<String> = peers
            .iter()
            .map(|p| format!("{}  ({})  {}", p.name, p.ip, p.os))
            .collect();
        match inquire::MultiSelect::new("Enlist which peers? (type to filter, space to toggle)", opts.clone())
            .prompt()
        {
            Ok(chosen) => chosen
                .iter()
                .filter_map(|c| opts.iter().position(|o| o == c))
                .collect(),
            Err(_) => return,
        }
    };

    if selected.is_empty() {
        println!("(none enlisted)");
        return;
    }

    let hub_url = format!("http://{hub_ip}:{port}");
    for idx in selected {
        let p = &peers[idx];
        let ssh_user = resolve_user(&p.name, user, dry_run);
        let target = match &ssh_user {
            Some(u) => format!("{u}@{}", p.ip),
            None => p.ip.clone(),
        };

        let dir = if dry_run {
            config::peer(&p.name).last_dir
        } else {
            pick_remote_dir(&target, &p.name)
        };

        // Remember the username and folder for next time.
        let mut pc = config::peer(&p.name);
        pc.user = ssh_user.clone();
        if let Some(d) = &dir {
            pc.last_dir = Some(d.clone());
        }
        let _ = config::set_peer(&p.name, pc);

        let script = remote_script(&hub_url, &p.name, dir.as_deref());

        if dry_run {
            println!("\n--- would run on {} ({}) ---\n{}", p.name, target, script);
            continue;
        }

        println!("\n→ enlisting {} ({})...", p.name, p.ip);
        match ssh_run(&target, &script) {
            Ok(s) if s.success() => {
                println!("  ✓ {} enlisted", p.name);
                println!("    watch:  claude.ai/code   |   attach:  ssh -t {target} tmux attach -t trident");
            }
            Ok(s) => println!("  ✗ {} failed (exit {:?})", p.name, s.code()),
            Err(e) => println!("  ✗ {} failed to reach over SSH: {e}", p.name),
        }
    }
}

/// Resolve the SSH login for a peer: --user flag wins, else the username saved
/// from a prior enlist is the Enter-default, else ask (blank = current user).
fn resolve_user(peer_name: &str, flag: Option<&str>, dry_run: bool) -> Option<String> {
    if let Some(u) = flag {
        return Some(u.to_string());
    }
    let saved = config::peer(peer_name).user;
    if dry_run || !stdin().is_terminal() {
        return saved;
    }
    let msg = format!("SSH username for {peer_name} (blank = current user):");
    let mut q = inquire::Text::new(&msg);
    if let Some(s) = &saved {
        q = q.with_default(s);
    }
    match q.prompt() {
        Ok(s) if s.trim().is_empty() => None,
        Ok(s) => Some(s.trim().to_string()),
        Err(_) => saved,
    }
}

/// The bootstrap script run on a peer: ensure trident is installed, then start
/// (or reuse) a `trident join` session inside tmux so it persists and so the
/// Claude TUI gets a PTY. Enlisted sessions are autonomous: --skip-perms on,
/// --rc on (watchable from the web), --yes (no prompts).
fn remote_script(hub_url: &str, name: &str, dir: Option<&str>) -> String {
    // tmux start-directory (the chosen project folder), if any.
    let cd = match dir {
        Some(d) => format!("-c \"{d}\" "),
        None => String::new(),
    };
    // The peer may already run a tmux server whose PATH lacks ~/.local/bin
    // (where both trident and claude install), so bake PATH into the command
    // rather than relying on the server's environment.
    format!(
        r#"set -e
export PATH="$HOME/.local/bin:$PATH"
command -v tmux >/dev/null 2>&1 || {{ echo "trident: tmux is not installed on this host"; exit 3; }}
# Update trident (best-effort) so `join --name` and the latest behavior exist.
curl -fsSL {install} | sh >/dev/null 2>&1 || command -v trident >/dev/null 2>&1 || {{ echo "trident: install failed"; exit 4; }}
TRIDENT="$(command -v trident || echo "$HOME/.local/bin/trident")"
if tmux has-session -t trident 2>/dev/null; then
  echo "trident: a session is already running here (reusing); tmux kill-session -t trident to restart"
else
  # Name comes from --name (persisted to config; claude does NOT pass env to the
  # MCP server). PATH is baked in because an existing tmux server's PATH may lack
  # ~/.local/bin (where trident and claude install). The initial prompt engages
  # the session so injected channel messages are processed without anyone attaching.
  tmux new -d -s trident {cd}"PATH=$HOME/.local/bin:$PATH $TRIDENT join {hub} --name {name} --yes --skip-perms on --rc on -- \"{prompt}\""
  # First run shows consent dialogs (folder trust, development channels). Enter
  # accepts the default 'proceed' so the headless session can start.
  for _ in 1 2 3 4 5 6; do sleep 2; tmux send-keys -t trident Enter 2>/dev/null || true; done
  echo "trident: started session '{name}'"
fi"#,
        install = INSTALL_URL,
        cd = cd,
        hub = hub_url,
        name = name,
        prompt = "You are a trident fleet worker session. Acknowledge briefly, then stand by and act on tasks sent by sibling sessions over the trident channel.",
    )
}

/// Connect to a peer and run the bootstrap. Prefers Tailscale SSH; on a
/// connection-level failure (e.g. the peer doesn't have Tailscale SSH enabled,
/// so `tailscale ssh` falls back to system ssh and trips host-key checking)
/// retries with plain ssh that accepts+saves the host key and installs our
/// public key, so the next enlist is passwordless. stdio is inherited so the
/// user can answer the one-time password prompt.
fn ssh_run(target: &str, script: &str) -> std::io::Result<std::process::ExitStatus> {
    // 1. Tailscale SSH (tailnet identity, no host-key prompts) when available.
    if let Some(ts) = tailscale_bin() {
        if let Ok(s) = Command::new(&ts).args(["ssh", target]).arg(script).status() {
            // exit 255 == ssh-level connection/auth failure → try plain ssh.
            // Any other code is a genuine remote result; surface it as-is.
            if s.code() != Some(255) {
                return Ok(s);
            }
            eprintln!("  (Tailscale SSH unavailable for this peer - falling back to ssh)");
        }
    }

    // 2. Plain ssh. accept-new saves an unknown host key (still rejects a
    //    CHANGED one). Prepend a key-install snippet for passwordless reuse.
    let payload = match local_pubkey() {
        Some(pk) => {
            eprintln!("  (you may be asked for a password once; installing a key for next time)");
            format!("{}\n{}", install_key_snippet(&pk), script)
        }
        None => script.to_string(),
    };
    Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=10",
            target,
        ])
        .arg(payload)
        .status()
}

/// Shell snippet that adds our public key to the peer's authorized_keys (idempotent).
fn install_key_snippet(pubkey: &str) -> String {
    format!(
        r#"mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
grep -qxF '{pk}' "$HOME/.ssh/authorized_keys" 2>/dev/null || printf '%s\n' '{pk}' >> "$HOME/.ssh/authorized_keys"
chmod 600 "$HOME/.ssh/authorized_keys" 2>/dev/null || true"#,
        pk = pubkey
    )
}

/// Read a local SSH public key, generating an ed25519 one if none exists.
fn local_pubkey() -> Option<String> {
    let ssh = config::home().join(".ssh");
    for name in ["id_ed25519.pub", "id_rsa.pub", "id_ecdsa.pub"] {
        if let Ok(s) = std::fs::read_to_string(ssh.join(name)) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    let _ = std::fs::create_dir_all(&ssh);
    let key = ssh.join("id_ed25519");
    let ok = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-f"])
        .arg(&key)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        eprintln!("  (generated {} for passwordless enlist)", key.display());
        std::fs::read_to_string(key.with_extension("pub"))
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        None
    }
}

fn online_peers() -> Vec<Peer> {
    let Some(v) = tailscale_status() else {
        return Vec::new();
    };
    let Some(map) = v["Peer"].as_object() else {
        return Vec::new();
    };
    let mut peers = Vec::new();
    for node in map.values() {
        if node["Online"].as_bool() != Some(true) {
            continue;
        }
        let name = node["HostName"].as_str().unwrap_or("peer").to_string();
        let ip = node["TailscaleIPs"]
            .as_array()
            .and_then(|a| a.iter().filter_map(|x| x.as_str()).find(|s| s.contains('.')))
            .unwrap_or("")
            .to_string();
        if ip.is_empty() {
            continue;
        }
        let os = node["OS"].as_str().unwrap_or("").to_string();
        peers.push(Peer { name, ip, os });
    }
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

// --- remote working-directory picker ---------------------------------------

/// Ask the peer which folder the session should run in: scan $HOME plus any
/// roots saved for this peer for git repos, present them, and remember the
/// pick. Returns None for the home directory (tmux default).
fn pick_remote_dir(target: &str, peer_name: &str) -> Option<String> {
    if !stdin().is_terminal() {
        return config::peer(peer_name).last_dir;
    }
    const HOME: &str = "~  (home directory)";
    const ADD: &str = "+ add a project root...";
    loop {
        let pc = config::peer(peer_name);
        let repos = discover_repos(target, &pc.roots);

        let mut opts = vec![HOME.to_string()];
        opts.extend(repos.iter().cloned());
        opts.push(ADD.to_string());

        let msg = format!("Working directory on {peer_name} (type to filter)");
        let mut select = inquire::Select::new(&msg, opts.clone()).with_page_size(15);
        // Start the cursor on last time's pick.
        if let Some(d) = &pc.last_dir {
            if let Some(pos) = opts.iter().position(|o| o == d) {
                select = select.with_starting_cursor(pos);
            }
        }

        match select.prompt() {
            Ok(choice) if choice == HOME => return None,
            Ok(choice) if choice == ADD => {
                if let Ok(root) =
                    inquire::Text::new(&format!("project root path on {peer_name}")).prompt()
                {
                    let root = root.trim().to_string();
                    if !root.is_empty() {
                        let mut pc = config::peer(peer_name);
                        if !pc.roots.contains(&root) {
                            pc.roots.push(root);
                            let _ = config::set_peer(peer_name, pc);
                        }
                    }
                }
                // loop to re-scan with the new root included
            }
            Ok(choice) => return Some(choice),
            Err(_) => return pc.last_dir, // cancelled → keep previous default
        }
    }
}

/// Find git repos on the peer under $HOME and the given extra roots.
fn discover_repos(target: &str, roots: &[String]) -> Vec<String> {
    let mut roots_sh = String::from("\"$HOME\"");
    for r in roots {
        let expanded = if let Some(rest) = r.strip_prefix("~/") {
            format!("$HOME/{rest}")
        } else if r == "~" {
            "$HOME".to_string()
        } else {
            r.clone()
        };
        roots_sh.push_str(&format!(" \"{expanded}\""));
    }
    let script = format!(
        r#"for r in {roots}; do [ -d "$r" ] && find "$r" -maxdepth 4 -type d -name .git -prune 2>/dev/null; done | sed "s:/\.git$::" | sort -u | head -40"#,
        roots = roots_sh
    );
    match ssh_capture(target, &script) {
        Some(out) => out.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => Vec::new(),
    }
}

/// Run a command on the peer and capture stdout. Non-interactive (BatchMode),
/// so it returns None instead of hanging on a password prompt.
fn ssh_capture(target: &str, script: &str) -> Option<String> {
    let try_capture = |mut cmd: Command| -> Option<String> {
        let out = cmd.arg(script).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    if let Some(ts) = tailscale_bin() {
        let mut c = Command::new(&ts);
        c.args(["ssh", target]);
        if let Some(s) = try_capture(c) {
            return Some(s);
        }
    }
    let mut c = Command::new("ssh");
    c.args([
        "-o", "BatchMode=yes",
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=10",
        target,
    ]);
    try_capture(c)
}

// --- Claude Code launch ----------------------------------------------------

fn launch_args(c: &Config, extra: &[String], sp: Option<bool>, rc: Option<bool>) -> Vec<String> {
    let mut args = vec![
        "--dangerously-load-development-channels".to_string(),
        "server:trident".to_string(),
    ];
    if rc.unwrap_or(c.rc) {
        args.push("--rc".to_string());
    }
    if sp.unwrap_or(c.skip_perms) {
        args.push("--dangerously-skip-permissions".to_string());
    }
    args.extend(extra.iter().cloned());
    args
}

fn launch_claude(
    c: &Config,
    extra: &[String],
    sp: Option<bool>,
    rc: Option<bool>,
    dry_run: bool,
) -> anyhow::Result<()> {
    let args = launch_args(c, extra, sp, rc);
    if dry_run {
        println!("claude {}", args.join(" "));
        return Ok(());
    }
    eprintln!("trident: launching claude {}", args.join(" "));
    match claude_command().args(&args).status() {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => anyhow::bail!("could not launch `claude` ({e}). Is Claude Code installed and on PATH?"),
    }
}

#[cfg(windows)]
fn claude_command() -> Command {
    let mut c = Command::new("cmd");
    c.arg("/c").arg("claude");
    c
}

#[cfg(not(windows))]
fn claude_command() -> Command {
    Command::new("claude")
}

// --- registration in ~/.claude.json ----------------------------------------

fn claude_json_path() -> PathBuf {
    config::home().join(".claude.json")
}

fn current_registration() -> Value {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "trident".to_string());
    json!({ "command": exe, "args": ["serve-mcp"] })
}

fn is_registered() -> bool {
    std::fs::read_to_string(claude_json_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["mcpServers"]["trident"].is_object())
        .unwrap_or(false)
}

/// True only if the registration already points at THIS binary's serve-mcp.
fn registration_current() -> bool {
    std::fs::read_to_string(claude_json_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["mcpServers"]["trident"] == current_registration())
        .unwrap_or(false)
}

pub fn ensure_registered(assume_yes: bool) -> anyhow::Result<()> {
    if registration_current() {
        return Ok(());
    }
    // A different/stale trident entry (e.g. an old node install with a hardcoded
    // hub) counts as already-consented - migrate it silently. Only a fresh
    // machine prompts.
    let stale = is_registered();
    if !stale
        && !assume_yes
        && !prompt_yes_no(
            "Register trident for all Claude Code sessions (writes ~/.claude.json)?",
            true,
        )
    {
        println!("Skipped. Run again or register later to enable the channel.");
        return Ok(());
    }

    let path = claude_json_path();
    let mut cfg: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));

    if !cfg["mcpServers"].is_object() {
        cfg["mcpServers"] = json!({});
    }
    cfg["mcpServers"]["trident"] = current_registration();

    if path.exists() {
        let _ = std::fs::copy(&path, path.with_extension("json.bak"));
    }
    std::fs::write(&path, serde_json::to_string_pretty(&cfg)?)?;
    if stale {
        println!("Updated trident registration to this binary in {}", path.display());
    } else {
        println!("Registered trident in {}", path.display());
    }

    if !stale
        && !assume_yes
        && prompt_yes_no(
            "Run trident sessions with --dangerously-skip-permissions by default? (lets remote sessions act autonomously)",
            false,
        )
    {
        let mut c = config::load();
        c.skip_perms = true;
        config::save(&c)?;
        println!("  skip_perms default = true (change with `trident config skip-perms off`)");
    }
    Ok(())
}

// --- small helpers ---------------------------------------------------------

fn prompt_yes_no(question: &str, default_yes: bool) -> bool {
    if !stdin().is_terminal() {
        return default_yes;
    }
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    let _ = stdout().flush();
    let mut line = String::new();
    if stdin().read_line(&mut line).is_err() {
        return default_yes;
    }
    match line.trim().to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    }
}

fn print_tailnet_hint() {
    if let Some(ip) = tailscale_ip() {
        println!("Other machines join with:  trident join http://{ip}:{}", config::hub_port());
    }
}

/// Locate the Tailscale CLI: on PATH, or the default Windows install location
/// (the GUI installer doesn't always add it to PATH).
fn tailscale_bin() -> Option<String> {
    if Command::new("tailscale")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some("tailscale".into());
    }
    #[cfg(windows)]
    {
        let p = r"C:\Program Files\Tailscale\tailscale.exe";
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    None
}

fn tailscale_ip() -> Option<String> {
    let ts = tailscale_bin()?;
    let out = Command::new(ts).args(["ip", "-4"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn tailscale_status() -> Option<Value> {
    let ts = tailscale_bin()?;
    let out = Command::new(ts).args(["status", "--json"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}
