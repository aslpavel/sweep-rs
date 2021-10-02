#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

use anyhow::{anyhow, Context, Error};
use argh::FromArgs;
use futures::TryStreamExt;
use std::{
    os::unix::{io::FromRawFd, net::UnixStream as StdUnixStream},
    pin::Pin,
    str::FromStr,
    sync::Arc,
};
use surf_n_term::widgets::Theme;
use sweep::{
    Candidate, FieldSelector, FuzzyScorer, Scorer, ScorerBuilder, SubstrScorer, Sweep, SweepEvent,
    SweepOptions, SCORER_NEXT_TAG,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args: Args = argh::from_env();

    if args.version {
        println!(
            "sweep {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("COMMIT_INFO")
        );
        return Ok(());
    }

    let (mut input, mut output): (
        Pin<Box<dyn AsyncRead + Send>>,
        Pin<Box<dyn AsyncWrite + Send>>,
    ) = match args.io_socket {
        None => {
            // Disabling `isatty` check on {stdin|stdout} on MacOS. When used
            // from asyncio python interface, sweep subprocess is created with
            // socketpair as its {stdin|stdout}, but `isatty` when used on socket
            // under MacOS causes "Operation not supported on socket" error.
            #[cfg(not(target_os = "macos"))]
            {
                use std::os::unix::io::AsRawFd;
                if nix::unistd::isatty(std::io::stdin().as_raw_fd())? {
                    return Err(anyhow!("stdin can not be a tty, pipe in data instead"));
                }
                if args.rpc && nix::unistd::isatty(std::io::stdout().as_raw_fd())? {
                    return Err(anyhow!("stdout can not be a tty if rpc is enabled"));
                }
            }
            (Box::pin(tokio::io::stdin()), Box::pin(tokio::io::stdout()))
        }
        Some(ref address) => {
            let stream = match address.parse() {
                Ok(fd) => unsafe { StdUnixStream::from_raw_fd(fd) },
                Err(_) => {
                    StdUnixStream::connect(&address).context("failed to connnect to io-socket")?
                }
            };
            stream.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(stream)?;
            let (input, output) = tokio::io::split(stream);
            (Box::pin(input), Box::pin(output))
        }
    };

    let sweep: Sweep<Candidate> = Sweep::new(SweepOptions {
        height: args.height,
        prompt: args.prompt.clone(),
        theme: args.theme.clone(),
        keep_order: args.keep_order,
        tty_path: args.tty_path.clone(),
        title: args.title.clone(),
        scorer_builder: args.scorer.toggle(),
        altscreen: args.altscreen,
        debug: args.debug,
    })?;
    sweep.niddle_set(args.query.clone());

    if args.rpc {
        sweep.serve(input, output).await?;
    } else {
        // TODO: create load future and wait for it
        if args.json {
            let mut data: Vec<u8> = Vec::new();
            tokio::io::copy(&mut input, &mut data).await?;
            let candidates: Vec<Candidate> =
                serde_json::from_slice(data.as_ref()).context("failed to parse input JSON")?;
            sweep.haystack_extend(candidates);
        } else {
            let sweep = sweep.clone();
            let field_dilimiter = args.field_delimiter;
            let field_selector = args.field_selector.clone();
            tokio::spawn(async move {
                let candidates = Candidate::from_lines(input, field_dilimiter, field_selector);
                tokio::pin!(candidates);
                while let Some(candidates) = candidates.try_next().await? {
                    sweep.haystack_extend(candidates);
                }
                Ok::<_, Error>(())
            });
        };
        let events = sweep.events();
        while let Ok(event) = events.recv() {
            match event {
                SweepEvent::Select(result) => {
                    if result.is_none() && !args.no_match_use_input {
                        continue;
                    }
                    let input = sweep.niddle_get().await?;
                    std::mem::drop(sweep); // cleanup terminal
                    let result = match result {
                        Some(candidate) if args.json => serde_json::to_string(&candidate)?,
                        Some(candidate) => candidate.to_string(),
                        None => input,
                    };
                    output.write_all(result.as_bytes()).await?;
                    break;
                }
                SweepEvent::Bind(tag) => {
                    if tag == SCORER_NEXT_TAG {
                        sweep.scorer_set(args.scorer.toggle());
                    }
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
pub struct ScorerSelector {
    scorers: Vec<ScorerBuilder>,
    index: usize,
}

impl Default for ScorerSelector {
    fn default() -> Self {
        Self::new(vec![
            Arc::new(|niddle: &str| {
                let niddle: Vec<_> = niddle.chars().flat_map(char::to_lowercase).collect();
                Arc::new(FuzzyScorer::new(niddle))
            }),
            Arc::new(|niddle: &str| {
                let niddle: Vec<_> = niddle.chars().flat_map(char::to_lowercase).collect();
                Arc::new(SubstrScorer::new(niddle))
            }),
        ])
    }
}

impl ScorerSelector {
    pub fn new(scorers: Vec<ScorerBuilder>) -> Self {
        if scorers.is_empty() {
            Default::default()
        } else {
            Self { scorers, index: 0 }
        }
    }

    pub fn name(&self) -> String {
        self.scorers[self.index]("").name().to_string()
    }

    pub fn toggle(&mut self) -> ScorerBuilder {
        let scorer = self.scorers[self.index].clone();
        self.index = (self.index + 1) % self.scorers.len();
        scorer
    }
}

impl FromStr for ScorerSelector {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let this = Self::default();
        let index = this
            .scorers
            .iter()
            .enumerate()
            .find_map(|(i, s)| if s("").name() == name { Some(i) } else { None })
            .ok_or_else(|| anyhow!("Unknown scorer: {}", name))?;
        Ok(Self { index, ..this })
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

    /// start sweep with the given query
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

    /// keep order (don't use ranking score)
    #[argh(switch, long = "keep-order")]
    pub keep_order: bool,

    /// default scorer to rank candidates
    #[argh(option, default = "ScorerSelector::default()")]
    pub scorer: ScorerSelector,

    /// enable debugging output
    #[argh(switch)]
    pub debug: bool,

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

    /// expect candidates in JSON format
    #[argh(switch)]
    pub json: bool,

    /// path/descriptor of the unix socket used to communicate instead of stdio/stdin
    #[argh(option)]
    pub io_socket: Option<String>,

    /// show sweep version and quit
    #[argh(switch)]
    pub version: bool,
}

fn parse_no_input(value: &str) -> Result<bool, String> {
    match value {
        "nothing" => Ok(false),
        "input" => Ok(true),
        _ => Err("invalid no-match achtion, possible values {nothing|input}".to_string()),
    }
}
