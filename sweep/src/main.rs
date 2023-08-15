#![deny(warnings)]
#![allow(clippy::type_complexity)]

use anyhow::{Context, Error};
use argh::FromArgs;
use futures::TryStreamExt;
use std::{
    collections::VecDeque,
    fs::File,
    io::Write,
    os::unix::{io::FromRawFd, net::UnixStream as StdUnixStream},
    pin::Pin,
    sync::{Arc, Mutex},
};
use sweep::{Candidate, FieldRefs, FieldSelector, Sweep, SweepEvent, SweepOptions, Theme};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tracing_subscriber::fmt::format::FmtSpan;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();

    if args.version {
        println!(
            "sweep {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("COMMIT_INFO")
        );
        return Ok(());
    }

    if let Some(log_path) = args.log {
        let log = Log::new(log_path)?;
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_span_events(FmtSpan::CLOSE)
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(move || log.clone())
            .init();
    }

    let (mut input, mut output): (
        Pin<Box<dyn AsyncRead + Send>>,
        Pin<Box<dyn AsyncWrite + Send>>,
    ) = match args.io_socket {
        None => {
            let input: Pin<Box<dyn AsyncRead + Send>> = match args.input.as_deref() {
                Some("-") | None => {
                    let stdin = tokio::io::stdin();
                    #[cfg(not(target_os = "macos"))]
                    {
                        use std::os::unix::io::AsRawFd;
                        if nix::unistd::isatty(stdin.as_raw_fd())? {
                            return Err(anyhow::anyhow!(
                                "stdin can not be a tty, pipe in data instead"
                            ));
                        }
                    }
                    Box::pin(stdin)
                }
                Some(path) => Box::pin(tokio::fs::File::open(path).await?),
            };

            // Disabling `isatty` check on {stdin|stdout} on MacOS. When used
            // from asyncio python interface, sweep subprocess is created with
            // `socketpair` as its {stdin|stdout}, but `isatty` when used on socket
            // under MacOS causes "Operation not supported on socket" error.
            #[cfg(not(target_os = "macos"))]
            {
                use std::os::unix::io::AsRawFd;
                if args.rpc && nix::unistd::isatty(std::io::stdout().as_raw_fd())? {
                    return Err(anyhow::anyhow!("stdout can not be a tty if rpc is enabled"));
                }
            }
            (input, Box::pin(tokio::io::stdout()))
        }
        Some(ref address) => {
            let stream = match address.parse() {
                Ok(fd) => unsafe { StdUnixStream::from_raw_fd(fd) },
                Err(_) => {
                    StdUnixStream::connect(address).context("failed to connnect to io-socket")?
                }
            };
            stream.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(stream)?;
            let (input, output) = tokio::io::split(stream);
            (Box::pin(input), Box::pin(output))
        }
    };

    let field_refs = FieldRefs::default();
    let sweep: Sweep<Candidate> = Sweep::new(
        field_refs.clone(),
        SweepOptions {
            height: args.height,
            prompt: args.prompt.clone(),
            theme: Theme {
                show_preview: args.preview,
                ..args.theme
            },
            keep_order: args.keep_order,
            tty_path: args.tty_path.clone(),
            title: args.title.clone(),
            scorers: VecDeque::new(),
            altscreen: args.altscreen,
            border: args.border,
            ..SweepOptions::default()
        },
    )?;
    sweep.query_set(args.query.clone());
    sweep.scorer_by_name(Some(args.scorer)).await?;

    if args.rpc {
        sweep
            .serve(input, output, |peer| Candidate::setup(peer, field_refs))
            .await?;
    } else {
        if args.json {
            let mut data: Vec<u8> = Vec::new();
            tokio::io::copy(&mut input, &mut data).await?;
            let candidates: Vec<Candidate> =
                serde_json::from_slice(data.as_ref()).context("failed to parse input JSON")?;
            sweep.items_extend(candidates);
        } else {
            let sweep = sweep.clone();
            let field_dilimiter = args.field_delimiter;
            let field_selector = args.field_selector.clone();
            tokio::spawn(async move {
                let candidates = Candidate::from_lines(input, field_dilimiter, field_selector);
                tokio::pin!(candidates);
                while let Some(candidates) = candidates.try_next().await? {
                    sweep.items_extend(candidates);
                }
                Ok::<_, Error>(())
            });
        };
        while let Some(event) = sweep.next_event().await {
            if let SweepEvent::Select(result) = event {
                if result.is_none() && !args.no_match_use_input {
                    continue;
                }
                let input = sweep.query_get().await?;
                std::mem::drop(sweep); // cleanup terminal
                let result = match result {
                    Some(candidate) if args.json => serde_json::to_string(&candidate)?,
                    Some(candidate) => candidate.to_string(),
                    None => input,
                };
                output.write_all(result.as_bytes()).await?;
                break;
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct Log {
    file: Arc<Mutex<File>>,
}

impl Log {
    fn new(file: String) -> Result<Self, Error> {
        let file = Arc::new(Mutex::new(File::create(file)?));
        Ok(Self { file })
    }
}

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = self.file.lock().expect("lock poisoned");
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut file = self.file.lock().expect("lock poisoned");
        file.flush()
    }
}

/// Sweep is a command line fuzzy finder
#[derive(FromArgs)]
pub struct Args {
    /// number of lines occupied by sweep
    #[argh(option, default = "11")]
    pub height: usize,

    /// prompt string
    #[argh(option, short = 'p', default = "\"INPUT\".to_string()")]
    pub prompt: String,

    /// initial query string
    #[argh(option, default = "String::new()")]
    pub query: String,

    /// theme as a list of comma-separated attributes
    #[argh(option, default = "Theme::light()")]
    pub theme: Theme,

    /// comma-separated list of fields for limiting search scope
    #[argh(option, long = "nth")]
    pub field_selector: Option<FieldSelector>,

    /// filed delimiter
    #[argh(option, long = "delimiter", short = 'd', default = "' '")]
    pub field_delimiter: char,

    /// keep order of items, that is only filter and do not sort
    #[argh(switch, long = "keep-order")]
    pub keep_order: bool,

    /// default scorer to rank items
    #[argh(option, from_str_fn(scorer_arg), default = "\"fuzzy\".to_string()")]
    pub scorer: String,

    /// use JSON-RPC protocol to communicate
    #[argh(switch)]
    pub rpc: bool,

    /// path to the TTY
    #[argh(option, long = "tty", default = "\"/dev/tty\".to_string()")]
    pub tty_path: String,

    /// action when there is no match and enter is pressed
    #[argh(
        option,
        long = "no-match",
        default = "false",
        from_str_fn(parse_no_input)
    )]
    pub no_match_use_input: bool,

    /// set terminal title
    #[argh(option, default = "\"sweep\".to_string()")]
    pub title: String,

    /// use alternative screen
    #[argh(switch)]
    pub altscreen: bool,

    /// expect candidates in JSON format (uses the same item format as RPC)
    #[argh(switch)]
    pub json: bool,

    /// path or file descriptor of the unix socket used to communicate instead of stdio/stdin
    #[argh(option)]
    pub io_socket: Option<String>,

    /// read input from the file instead of stdin, ignored if --io-socket is used
    #[argh(option)]
    pub input: Option<String>,

    /// show sweep version and quit
    #[argh(switch)]
    pub version: bool,

    /// enable logging into specified file path, logging verbosity is configure with RUST_LOG
    #[argh(option)]
    pub log: Option<String>,

    /// leave border on the sides
    #[argh(option, default = "1")]
    pub border: usize,

    /// whether to show item preview by default or not
    #[argh(option, default = "true")]
    pub preview: bool,
}

fn parse_no_input(value: &str) -> Result<bool, String> {
    match value {
        "nothing" => Ok(false),
        "input" => Ok(true),
        _ => Err("invalid no-match achtion, possible values {nothing|input}".to_string()),
    }
}

fn scorer_arg(name: &str) -> Result<String, String> {
    match name {
        "substr" => Ok(name.to_string()),
        "fuzzy" => Ok(name.to_string()),
        _ => Err(format!("unknown scorer type: {}", name)),
    }
}
