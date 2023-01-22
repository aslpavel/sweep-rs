mod history;
mod navigator;
mod utils;
mod walk;

use anyhow::Error;
use history::History;
use navigator::{Navigator, NavigatorState};
use sweep::{SweepOptions, Theme};

use std::{io::Read, path::PathBuf};
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

const HISTORY_DB: &str = ".command_history.db";

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();

    let appnder = tracing_appender::rolling::never("/tmp", "hist.log");
    tracing_subscriber::fmt()
        .with_span_events(FmtSpan::CLOSE)
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(appnder)
        .init();

    let db_path = args
        .db
        .or_else(|| Some(dirs::home_dir()?.join(HISTORY_DB)))
        .ok_or_else(|| anyhow::anyhow!("faield to determine home directory"))?;
    let options = SweepOptions {
        theme: args.theme,
        ..Default::default()
    };

    match args.subcommand {
        ArgsSubcommand::Cmd(_args) => {
            let mut navigator =
                Navigator::new(options, db_path, NavigatorState::CmdHistory).await?;
            let entry = navigator.run().await?;
            std::mem::drop(navigator);
            if let Some(entry) = entry {
                println!("{}", entry);
            }
        }
        ArgsSubcommand::Update(_args) => {
            let history = History::new(db_path).await?;
            let mut update_str = String::new();
            std::io::stdin().read_to_string(&mut update_str)?;
            history.update(update_str.parse()?).await?;
            history.close().await?;
        }
        ArgsSubcommand::Path(args) => {
            let mut navigator = match args.path {
                None => Navigator::new(options, db_path, NavigatorState::PathHistory).await?,
                Some(path) => {
                    Navigator::new(options, db_path, NavigatorState::Path(path.canonicalize()?))
                        .await?
                }
            };
            let entry = navigator.run().await?;
            std::mem::drop(navigator);
            if let Some(entry) = entry {
                println!("{}", entry);
            }
        }
    }
    Ok(())
}

/// Select entry from the cmd history database
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "cmd")]
struct ArgsCmd {}

/// Update entry in the history database
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "update")]
struct ArgsUpdate {}

/// List path
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "path")]
struct ArgsPath {
    /// path that will be listed
    #[argh(positional)]
    path: Option<PathBuf>,
}

#[derive(Debug, argh::FromArgs)]
#[argh(subcommand)]
enum ArgsSubcommand {
    Cmd(ArgsCmd),
    Update(ArgsUpdate),
    Path(ArgsPath),
}

/// History manager
#[derive(Debug, argh::FromArgs)]
struct Args {
    /// history database path
    #[argh(option)]
    db: Option<PathBuf>,
    /// theme as a list of comma-separated attributes
    #[argh(option, default = "Theme::light()")]
    pub theme: Theme,
    /// action
    #[argh(subcommand)]
    subcommand: ArgsSubcommand,
}
