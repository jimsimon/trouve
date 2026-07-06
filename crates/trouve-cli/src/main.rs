//! `trouve` — terminal client for the trouve harness.
//!
//! Every command except `serve` talks to a running server over the protocol
//! (invariant 1); `serve` embeds the server the same way the desktop app
//! spawns one.

mod auth;
mod bridge;
mod client;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "trouve", version, about = "AI coding harness")]
struct Cli {
    /// Server base URL.
    #[arg(long, global = true, default_value = "http://127.0.0.1:7433")]
    server: String,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the harness server.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7433")]
        addr: std::net::SocketAddr,
    },
    /// Manage workspaces (registered repositories).
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    /// Manage sessions (worktree + branch per session).
    #[command(subcommand)]
    Session(SessionCmd),
    /// Manage provider credentials (stored in the OS keychain).
    #[command(subcommand)]
    Auth(AuthCmd),
    /// List models available from configured providers.
    Models,
    /// Chat in a new thread of a session.
    Chat {
        /// Session id.
        #[arg(long)]
        session: String,
        /// Agent mode (code, plan, review, architect, question).
        #[arg(long)]
        mode: Option<String>,
        /// Provider-qualified model, e.g. openai/gpt-4.1-mini.
        #[arg(long)]
        model: Option<String>,
        /// Permission mode: ask, allow_list, or yolo.
        #[arg(long)]
        permissions: Option<String>,
    },
    /// Stdio MCP server bridging trouve's tools into an external vendor
    /// agent (launched by the engine, not by hand). Reads TROUVE_SERVER and
    /// TROUVE_THREAD_ID from the environment.
    #[command(hide = true)]
    McpBridge,
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Store an API key for a provider (prompted on stdin).
    SetKey { provider: String },
    /// Log in with OAuth (device flow, or browser PKCE when configured).
    Login { provider: String },
    /// Remove stored credentials for a provider.
    Logout { provider: String },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Register a git repository as a workspace.
    Add { path: String },
    /// List registered workspaces.
    List,
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Create a session (new worktree + branch) in a workspace.
    New {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        base_ref: Option<String>,
    },
    /// List sessions.
    List,
    /// Restore the previous checkpoint.
    Undo { id: String },
    /// Re-apply the next checkpoint.
    Redo { id: String },
    /// Delete a session (removes its worktree; keeps the branch).
    Delete { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Serve { addr } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            let data = trouve_core::config::data_dir();
            let store = trouve_core::store::Store::open(&data.join("trouve.db"))?;
            let config = trouve_core::config::Config::load();
            let engine = std::sync::Arc::new(trouve_core::Engine::new(store, data, &config));
            trouve_server::serve(engine, addr).await
        }
        Cmd::Workspace(cmd) => {
            let api = client::Api::new(cli.server);
            match cmd {
                WorkspaceCmd::Add { path } => {
                    let abs = std::fs::canonicalize(&path)?;
                    let ws = api.register_workspace(abs.to_str().unwrap()).await?;
                    println!(
                        "{}  {}  {}",
                        ws["id"].as_str().unwrap(),
                        ws["name"].as_str().unwrap(),
                        ws["path"].as_str().unwrap()
                    );
                }
                WorkspaceCmd::List => {
                    for ws in api.list_workspaces().await? {
                        println!(
                            "{}  {}  {}",
                            ws["id"].as_str().unwrap(),
                            ws["name"].as_str().unwrap(),
                            ws["path"].as_str().unwrap()
                        );
                    }
                }
            }
            Ok(())
        }
        Cmd::Session(cmd) => {
            let api = client::Api::new(cli.server);
            match cmd {
                SessionCmd::New {
                    workspace,
                    title,
                    base_ref,
                } => {
                    let s = api
                        .create_session(&workspace, title.as_deref(), base_ref.as_deref())
                        .await?;
                    println!(
                        "{}  branch={}  worktree={}",
                        s["id"].as_str().unwrap(),
                        s["branch"].as_str().unwrap(),
                        s["worktree_path"].as_str().unwrap()
                    );
                }
                SessionCmd::List => {
                    for s in api.list_sessions().await? {
                        println!(
                            "{}  [{}]  {}  {}",
                            s["id"].as_str().unwrap(),
                            s["workspace_id"].as_str().unwrap(),
                            s["title"].as_str().unwrap(),
                            s["branch"].as_str().unwrap()
                        );
                    }
                }
                SessionCmd::Undo { id } => api.undo(&id).await?,
                SessionCmd::Redo { id } => api.redo(&id).await?,
                SessionCmd::Delete { id } => api.delete_session(&id).await?,
            }
            Ok(())
        }
        Cmd::Auth(cmd) => match cmd {
            AuthCmd::SetKey { provider } => auth::set_key(&provider).await,
            AuthCmd::Login { provider } => auth::login(&provider).await,
            AuthCmd::Logout { provider } => auth::logout(&provider),
        },
        Cmd::Models => {
            let api = client::Api::new(cli.server);
            for m in api.list_models().await? {
                println!(
                    "{}  ctx={}  in=${}/M out=${}/M",
                    m["id"].as_str().unwrap_or("?"),
                    m["context_window"],
                    m["input_price_per_mtok"],
                    m["output_price_per_mtok"],
                );
            }
            Ok(())
        }
        Cmd::Chat {
            session,
            mode,
            model,
            permissions,
        } => {
            let api = client::Api::new(cli.server);
            client::chat(
                &api,
                &session,
                mode.as_deref(),
                model.as_deref(),
                permissions.as_deref(),
            )
            .await
        }
        Cmd::McpBridge => bridge::run().await,
    }
}
