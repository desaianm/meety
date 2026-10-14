use crate::{app, mcp, model, store::Paths, transcribe};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Private offline meeting transcripts. Launch without arguments for the menu bar app."
)]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Run the native menu bar app; does not start recording automatically.
    App,
    /// Start recording through the running app.
    Start {
        #[arg(long)]
        title: Option<String>,
    },
    /// Stop recording and transcribe locally in the background.
    Stop,
    /// Recording state and source packet counts.
    Status,
    /// List locally stored meetings as JSON.
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Permanently delete a meeting's notes, transcript and saved audio.
    Delete {
        id: String,
        /// Required confirmation; deletion cannot be undone.
        #[arg(long)]
        yes: bool,
    },
    /// Read a transcript (use 'latest' for the most recent).
    Get {
        #[arg(default_value = "latest")]
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Search transcript text locally. Returns JSON.
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Write one transcript to a Markdown file.
    Export {
        #[arg(default_value = "latest")]
        id: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Import and transcribe a PCM WAV file locally.
    Import {
        file: PathBuf,
        #[arg(long)]
        title: Option<String>,
    },
    /// Retry a failed/interrupted transcription using retained audio.
    Retry {
        #[arg(default_value = "latest")]
        id: String,
    },
    /// English model download and selection (only this command needs internet).
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Read-only MCP server for Claude Code and Codex, over stdio.
    Mcp,
    /// Show app, model and data paths; never records audio.
    Doctor,
    /// Print agent registration commands; makes no configuration changes.
    AgentSetup,
}
#[derive(Subcommand)]
enum ModelAction {
    Install {
        #[arg(value_enum, default_value = "medium")]
        model: model::Choice,
    },
    Use {
        #[arg(value_enum)]
        model: model::Choice,
    },
    Info,
    Manifest {
        #[arg(value_enum, default_value = "medium")]
        model: model::Choice,
    },
}
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let p = Paths::new(cli.data_dir)?;
    match cli.command.unwrap_or(Command::App) {
        Command::App => app::run(p)?,
        Command::Start { title } => print_json(&app::request(
            &p,
            serde_json::json!({"op":"start","title":title}),
        )?)?,
        Command::Stop => print_json(&app::request(&p, serde_json::json!({"op":"stop"}))?)?,
        Command::Status => print_json(&app::request(&p, serde_json::json!({"op":"status"}))?)?,
        Command::List { limit } => print_json(&p.list(limit)?)?,
        Command::Delete { id, yes } => {
            if !yes {
                bail!("Deletion cannot be undone. Confirm with: meetlocal delete {id} --yes");
            }
            let id = p.resolve(&id)?;
            print_json(&app::request(
                &p,
                serde_json::json!({"op":"delete","id":id}),
            )?)?;
        }
        Command::Get { id, json } => {
            let t = p.get(&id)?;
            if json {
                print_json(&t)?;
            } else {
                print!("{}", t.markdown());
            }
        }
        Command::Search { query, limit } => print_json(&p.search(&query, limit)?)?,
        Command::Export { id, output } => {
            let t = p.get(&id)?;
            crate::store::atomic_write(&output, t.markdown().as_bytes())?;
            print_json(&serde_json::json!({"path":output,"id":t.meeting.id}))?;
        }
        Command::Import { file, title } => {
            let title = title.unwrap_or_else(|| {
                file.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into()
            });
            let id = transcribe::import(&p, &file, &title)?;
            print_json(&serde_json::json!({"id":id,"status":"complete"}))?;
        }
        Command::Retry { id } => {
            let id = p.resolve(&id)?;
            transcribe::run(&p, &id)?;
            print_json(&serde_json::json!({"id":id,"status":"complete"}))?;
        }
        Command::Model { action } => match action {
            ModelAction::Install { model: choice } => {
                model::install(&p, choice)?;
                print_json(&serde_json::json!({"model":choice,"ready":true}))?;
            }
            ModelAction::Use { model: choice } => {
                if !model::ready(&p, choice) {
                    bail!("Run meetlocal model install {} first", choice.name());
                }
                model::select(&p, choice)?;
            }
            ModelAction::Info => {
                let c = model::selected(&p);
                print_json(
                    &serde_json::json!({"model":c,"ready":model::ready(&p,c),"path":model::path(&p,c)}),
                )?;
            }
            ModelAction::Manifest { model: choice } => println!("{}", model::dependencies(choice)?),
        },
        Command::Mcp => mcp::run(&p)?,
        Command::Doctor => {
            let choice = model::selected(&p);
            // A shell-launched process may inherit its terminal's TCC identity.
            // Ask the app for its permission state instead of reporting that
            // parent process's access as Meetlocal's access.
            let app_status = app::request(&p, serde_json::json!({"op":"status"})).ok();
            let microphone_permission = app_status
                .as_ref()
                .and_then(|s| s["microphone_permission"].as_str())
                .unwrap_or("app_not_running");
            print_json(
                &serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"data_directory":p.root,"database":p.db_path(),"model":choice,"model_ready":model::ready(&p,choice),"app_running":app_status.is_some(),"microphone_permission":microphone_permission,"permissions":"Start Recording requests Microphone and System Audio Recording. macOS 14.2+ required.","storage":"Local SQLite, Markdown, and retained WAV. No cloud or expiration."}),
            )?;
        }
        Command::AgentSetup => {
            let executable = std::env::current_exe()?;
            let quoted = format!(
                "'{}'",
                executable.display().to_string().replace('\'', "'\\''")
            );
            println!("claude mcp add --transport stdio --scope user meetlocal -- {quoted} mcp\ncodex mcp add meetlocal -- {quoted} mcp\n\nBoth integrations expose only list, get, search and status. Recording is controlled through the menu or CLI.");
        }
    }
    Ok(())
}
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
