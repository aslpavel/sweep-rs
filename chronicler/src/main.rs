#![deny(warnings)]

mod history;
mod navigator;
mod utils;
mod walk;

use anyhow::Error;
use history::History;
use navigator::{CmdHistoryMode, Navigator, NavigatorItem, PathHistoryMode, PathMode};
use sweep::{SweepOptions, Theme};
use time::{format_description::FormatItem, macros::format_description};

use std::{io::Read, path::PathBuf};
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

const HISTORY_DB: &str = "chronicler/history.db";

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DATE_FORMAT: &[FormatItem<'_>] =
    format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3]");

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();

    if args.version {
        println!(
            "chronicler {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("COMMIT_INFO")
        );
        return Ok(());
    }

    // setup log
    if let Some(mut cache_dir) = dirs::cache_dir() {
        cache_dir.push("chronicler");
        let appnder = tracing_appender::rolling::never(cache_dir, "chronicler.log");
        tracing_subscriber::fmt()
            .with_span_events(FmtSpan::CLOSE)
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(appnder)
            .init();
    }

    let db_path = args
        .db
        .or_else(|| Some(dirs::data_dir()?.join(HISTORY_DB)))
        .ok_or_else(|| anyhow::anyhow!("faield to determine home directory"))?;
    let db_dir = db_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("failed determine db directory"))?;
    if !db_dir.exists() {
        std::fs::create_dir_all(db_dir)?;
    }

    let options = SweepOptions {
        theme: args.theme,
        tty_path: args.tty_path,
        ..Default::default()
    };
    let query = (!args.query.is_empty()).then_some(args.query.as_ref());

    match args.subcommand {
        ArgsSubcommand::Cmd(_args) => {
            let mut navigator = Navigator::new(options, db_path).await?;
            let items = navigator.run(query, CmdHistoryMode::new(None)).await?;
            std::mem::drop(navigator);
            print_items(&items);
        }
        ArgsSubcommand::Path(args) => {
            let mut navigator = Navigator::new(options, db_path).await?;
            let mode = match args.path {
                None => PathHistoryMode::new(),
                Some(path) => PathMode::new(path.canonicalize()?, String::new()),
            };
            let items = navigator.run(query, mode).await?;
            std::mem::drop(navigator);
            print_items(&items);
        }
        ArgsSubcommand::Update(args) if args.show_db_path => {
            print!("{}", db_path.to_string_lossy())
        }
        ArgsSubcommand::Update(args) => {
            let history = History::new(db_path).await?;
            let mut update_str = String::new();
            std::io::stdin().read_to_string(&mut update_str)?;
            let update = if args.json {
                serde_json::from_str(&update_str)?
            } else {
                update_str.parse()?
            };
            let id = history.update(update).await?;
            history.close().await?;
            print!("{id}")
        }
        ArgsSubcommand::Setup(args) => {
            const CHRONICLER_PATTERN: &str = "##CHRONICLER_BIN##";
            let chronicler_path = std::env::current_exe()?;
            let chronicler_bin = chronicler_path.to_str().unwrap_or("chronicler");
            let setup = match args.shell {
                Shell::Bash => {
                    include_str!("../scripts/setup.sh").replace(CHRONICLER_PATTERN, chronicler_bin)
                }
            };
            print!("{setup}")
        }
    }
    Ok(())
}

fn print_items(items: &[NavigatorItem]) {
    for (index, item) in items.iter().enumerate() {
        print!("{}={}", item.tag(), item);
        if index + 1 != items.len() {
            print!("\x0c")
        }
    }
}

/// Select entry from the cmd history database
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "cmd")]
struct ArgsCmd {}

/// Update entry in the history database
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "update")]
struct ArgsUpdate {
    /// return path to the database
    #[argh(switch)]
    show_db_path: bool,

    /// json input format
    #[argh(switch)]
    json: bool,
}

/// List path
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "path")]
struct ArgsPath {
    /// path that will be listed
    #[argh(positional)]
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
enum Shell {
    Bash,
}

impl std::str::FromStr for Shell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "bash" | "sh" => Ok(Shell::Bash),
            _ => Err(format!("failed to parse shell type: {s}")),
        }
    }
}

/// Output script that will setup chronicler
#[derive(Debug, argh::FromArgs)]
#[argh(subcommand, name = "setup")]
struct ArgsSetup {
    #[argh(positional)]
    shell: Shell,
}

#[derive(Debug, argh::FromArgs)]
#[argh(subcommand)]
enum ArgsSubcommand {
    Cmd(ArgsCmd),
    Update(ArgsUpdate),
    Path(ArgsPath),
    Setup(ArgsSetup),
}

/// History manager
#[derive(Debug, argh::FromArgs)]
struct Args {
    /// show sweep version and quit
    #[argh(switch)]
    pub version: bool,

    /// history database path
    #[argh(option)]
    db: Option<PathBuf>,

    /// theme as a list of comma-separated attributes
    #[argh(option, default = "Theme::from_env()")]
    pub theme: Theme,

    /// path to the TTY
    #[argh(option, long = "tty", default = "\"/dev/tty\".to_string()")]
    pub tty_path: String,

    /// initial query
    #[argh(option, long = "query", default = "String::new()")]
    pub query: String,

    /// action
    #[argh(subcommand)]
    subcommand: ArgsSubcommand,
}
