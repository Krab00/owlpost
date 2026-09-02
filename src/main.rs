//! clap entry; dispatch to subcommands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

use owlpost::{config, contacts, daemon, identity};

#[derive(Parser)]
#[command(
    name = "owl",
    version,
    about = "owlpost — ask your peers' agents about their code"
)]
struct Cli {
    /// Owlpost home directory (overrides $OWLPOST_HOME)
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

// ponytail: args are minimal placeholders — each later task fills in its own subcommand.
#[derive(Subcommand)]
enum Cmd {
    /// Create home, key, config; print fingerprint
    Init {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Vec<String>,
    },
    /// Identity summary
    Whoami,
    /// Print own card, or fetch and print a peer's card
    Card { peer: Option<String> },
    /// Merged contact book: list | export | show <peer>
    Contact {
        #[command(subcommand)]
        cmd: ContactCmd,
    },
    /// TOFU-add a peer to the local provider
    Add {
        host_port: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// Set policy manual (once) or auto (always)
    Allow {
        peer: String,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        always: bool,
        #[arg(long)]
        i_verified_the_fingerprint: bool,
    },
    /// Set policy never
    Deny { peer: String },
    /// Send a question to a peer
    Ask(cli::ask::AskArgs),
    /// List or count inbox records
    Inbox {
        #[arg(long)]
        count: bool,
        #[arg(long)]
        new: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        format: Option<String>,
    },
    /// Full content of a record; marks it seen
    Show { id: String },
    /// Run the responder and store a draft
    Draft {
        id: String,
        #[arg(long)]
        harness: Option<String>,
    },
    /// Open the draft in $EDITOR
    Edit { id: String },
    /// Sign and move to outbox
    Send { id: String },
    /// Discard a record
    Reject { id: String },
    /// Finished exchanges
    History {
        #[arg(long)]
        peer: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    /// Block until a matching inbox record arrives
    Watch {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Run the listener and loops
    Daemon {
        /// Stay attached to the terminal (currently the only mode; `owl install` supervises)
        #[arg(long)]
        foreground: bool,
    },
    /// Install the launchd/systemd service
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall the service
    Uninstall,
    /// Check key, config, endpoints, harnesses, daemon
    Doctor,
}

#[derive(Subcommand)]
enum ContactCmd {
    /// One row per merged contact: name, fingerprint, source, policy
    List,
    /// Print one contact as JSON
    Show { peer: String },
    /// This machine's repo-provider entry (from config + key) as JSON
    Export,
}

fn init(home: &Path, name: Option<String>, emails: Vec<String>) -> anyhow::Result<()> {
    // Check the key BEFORE touching config so a refused init leaves the home untouched.
    if home.join("key").exists() {
        anyhow::bail!(
            "key already exists at {} — refusing to overwrite",
            home.join("key").display()
        );
    }
    let mut cfg = config::Config::load(home)?;
    if let Some(n) = name {
        cfg.name = n;
    }
    if !emails.is_empty() {
        cfg.emails = emails;
    }
    cfg.save(home)?;
    let id = identity::Identity::generate();
    id.save(home)?;
    println!("{}", identity::fingerprint(&id.verifying_key()));
    Ok(())
}

fn whoami(home: &Path, json: bool) -> anyhow::Result<()> {
    if !config::Config::path(home).exists() || !home.join("key").exists() {
        anyhow::bail!(
            "no identity found in {} — run `owl init` first",
            home.display()
        );
    }
    let cfg = config::Config::load(home)?;
    let id = identity::Identity::load(home)?;
    let pk = id.verifying_key();
    let fp = identity::fingerprint(&pk);
    let pubkey = identity::pubkey_string(&pk);
    if json {
        let out = serde_json::json!({
            "fingerprint": fp,
            "pubkey": pubkey,
            "name": cfg.name,
            "endpoints": cfg.endpoints,
        });
        println!("{out}");
    } else {
        println!("fingerprint: {fp}");
        println!("pubkey: {pubkey}");
        println!("name: {}", cfg.name);
        println!("endpoints: {}", cfg.endpoints.join(", "));
    }
    Ok(())
}

fn require_identity(home: &Path) -> anyhow::Result<(config::Config, identity::Identity)> {
    if !config::Config::path(home).exists() || !home.join("key").exists() {
        anyhow::bail!(
            "no identity found in {} — run `owl init` first",
            home.display()
        );
    }
    Ok((config::Config::load(home)?, identity::Identity::load(home)?))
}

fn contact(home: &Path, cmd: ContactCmd, json: bool) -> anyhow::Result<()> {
    let book = || contacts::ContactBook::load(home, &std::env::current_dir()?);
    match cmd {
        ContactCmd::List => {
            let book = book()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&book.contacts)?);
            } else {
                println!(
                    "{:<20} {:<20} {:<6} POLICY",
                    "NAME", "FINGERPRINT", "SOURCE"
                );
                for c in &book.contacts {
                    let mode = c.policy.as_ref().map_or("-", |p| p.mode.as_str());
                    println!(
                        "{:<20} {:<20} {:<6} {mode}",
                        c.name, c.fingerprint, c.source
                    );
                }
            }
        }
        ContactCmd::Show { peer } => {
            let book = book()?;
            println!("{}", serde_json::to_string_pretty(book.resolve(&peer)?)?);
        }
        ContactCmd::Export => {
            let (cfg, id) = require_identity(home)?;
            let out = serde_json::json!({
                "name": cfg.name,
                "emails": cfg.emails,
                "pubkey": identity::pubkey_string(&id.verifying_key()),
                "endpoints": cfg.endpoints,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(())
}

/// `owl daemon [--foreground]`. Both forms run attached: detaching is the service manager's
/// job (`owl install`, OWL-012), so the flag documents intent rather than changing behaviour.
// ponytail: no self-daemonising — launchd/systemd keep the process in the foreground anyway.
fn daemon_cmd(home: &Path, _foreground: bool) -> anyhow::Result<()> {
    let (cfg, _id) = require_identity(home)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(daemon::run_foreground(home, cfg))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let home = config::home_dir(cli.home.as_deref());
    let result = match cli.cmd {
        Cmd::Init { name, email } => init(&home, name, email),
        Cmd::Whoami => whoami(&home, cli.json),
        Cmd::Contact { cmd } => contact(&home, cmd, cli.json),
        Cmd::Daemon { foreground } => daemon_cmd(&home, foreground),
        Cmd::Inbox {
            count,
            new,
            all,
            format,
        } => cli::inbox::run(
            &home,
            cli::inbox::Opts {
                count,
                new,
                all,
                format,
                json: cli.json,
            },
        ),
        Cmd::Show { id } => cli::show::run(&home, &id, cli.json),
        Cmd::Draft { id, harness } => cli::draft::run(&home, &id, harness.as_deref(), cli.json),
        Cmd::Edit { id } => cli::edit::run(&home, &id, cli.json),
        Cmd::Send { id } => cli::send::run(&home, &id, cli.json),
        Cmd::Reject { id } => cli::reject::run(&home, &id, cli.json),
        Cmd::History { peer, path, since } => {
            cli::history::run(&home, cli::history::Filters { peer, path, since }, cli.json)
        }
        Cmd::Ask(args) => cli::ask::run(&home, args, cli.json, cli.quiet),
        Cmd::Watch { id, timeout } => cli::watch::run(&home, id.as_deref(), timeout),
        Cmd::Install { dry_run } => cli::install::install(&home, dry_run),
        Cmd::Uninstall => cli::install::uninstall(),
        Cmd::Doctor => cli::doctor::run(&home, cli.json),
        _ => Err(anyhow::anyhow!("not implemented yet")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("owl: {e:#}");
            ExitCode::from(cli::exit_code(&e))
        }
    }
}
