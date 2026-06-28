// trident - bridge sibling Claude Code sessions across machines.
//
// One static binary, several modes:
//   trident join [hub]   register the channel + launch a Claude Code session
//   trident host         become the hub + launch a session (orchestration TODO)
//   trident config ...    view/change hub, name, and launch defaults
//   trident serve-mcp    (hidden) the stdio MCP channel server Claude Code spawns
//   trident hub          (hidden) the broker that routes messages between sessions

mod cli;
mod config;
mod hub;
mod mcp;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trident", version, about = "Bridge sibling Claude Code sessions across machines")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect this machine to a hub and launch a Claude Code session
    Join {
        /// Hub URL to point at, e.g. http://100.x.y.z:8790 (persisted to config)
        hub: Option<String>,
        /// This session's roster name (persisted to config)
        #[arg(long)]
        name: Option<String>,
        /// Don't prompt before registering the channel (for scripted/remote use)
        #[arg(long)]
        yes: bool,
        /// Override the skip-permissions default for this launch
        #[arg(long)]
        skip_perms: Option<OnOff>,
        /// Override the remote-control (--rc) default for this launch
        #[arg(long)]
        rc: Option<OnOff>,
        /// Print the claude invocation instead of running it
        #[arg(long)]
        dry_run: bool,
        /// Extra arguments passed through to `claude` (after `--`)
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
    /// Become the hub, enlist tailnet peers, and launch a session
    Host {
        /// Skip the interactive peer picker (just be the hub + launch locally)
        #[arg(long)]
        no_enlist: bool,
        /// SSH login to use for enlisted peers (default: tailnet/current user)
        #[arg(long)]
        user: Option<String>,
        /// Print what would run (claude command + per-peer SSH scripts)
        #[arg(long)]
        dry_run: bool,
        /// Extra arguments passed through to `claude` (after `--`)
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
    /// List online tailnet peers (name, ip, os)
    Peers,
    /// Spawn a trident worker session on a remote peer (non-interactive)
    Enlist {
        /// Peer name or IP (from `trident peers`)
        peer: String,
        /// Working directory on the peer (default: saved, else home)
        #[arg(long)]
        dir: Option<String>,
        /// SSH login for the peer (default: saved, else current user)
        #[arg(long)]
        user: Option<String>,
    },
    /// Install the trident slash commands into ~/.claude/commands
    InstallCommands,
    /// View or change hub, name, and launch defaults
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },
    /// (internal) stdio MCP channel server spawned by Claude Code
    #[command(hide = true)]
    ServeMcp,
    /// (internal) message broker; sessions connect here to reach each other
    #[command(hide = true)]
    Hub,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Show the current configuration and resolved launch command
    Show,
    /// Point this machine at a hub URL
    SetHub { url: String },
    /// This machine is the hub (sessions use localhost; broker binds 0.0.0.0)
    Host,
    /// Set this session's roster name
    SetName { name: String },
    /// Default --dangerously-skip-permissions on/off
    SkipPerms { value: OnOff },
    /// Default --rc (remote control) on/off
    Rc { value: OnOff },
}

#[derive(Clone, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl OnOff {
    fn as_bool(&self) -> bool {
        matches!(self, OnOff::On)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::ServeMcp) => mcp::run().await,
        Some(Cmd::Hub) => hub::run().await,
        Some(Cmd::Join { hub, name, yes, skip_perms, rc, dry_run, claude_args }) => cli::join(
            hub,
            name,
            yes,
            skip_perms.map(|v| v.as_bool()),
            rc.map(|v| v.as_bool()),
            dry_run,
            claude_args,
        ),
        Some(Cmd::Host { no_enlist, user, dry_run, claude_args }) => {
            cli::host(dry_run, no_enlist, user, claude_args)
        }
        Some(Cmd::Peers) => {
            cli::peers();
            Ok(())
        }
        Some(Cmd::Enlist { peer, dir, user }) => cli::enlist_one(peer, dir, user),
        Some(Cmd::InstallCommands) => {
            cli::install_commands(true);
            Ok(())
        }
        Some(Cmd::Config { cmd }) => {
            match cmd.unwrap_or(ConfigCmd::Show) {
                ConfigCmd::Show => cli::config_show(),
                ConfigCmd::SetHub { url } => cli::config_set_hub(url)?,
                ConfigCmd::Host => cli::config_host_mode()?,
                ConfigCmd::SetName { name } => cli::config_set_name(name)?,
                ConfigCmd::SkipPerms { value } => cli::config_set_skip_perms(value.as_bool())?,
                ConfigCmd::Rc { value } => cli::config_set_rc(value.as_bool())?,
            }
            Ok(())
        }
        // Bare `trident`: be the hub + launch a normal session.
        None => cli::start(Vec::new()),
    }
}
