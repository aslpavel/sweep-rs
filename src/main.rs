#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

use anyhow::{anyhow, Context, Error};
use argh::FromArgs;
use crossbeam_channel::{never, select, unbounded};
use serde_json::{self, Value};
use std::{
    io::{Read, Write},
    os::unix::{io::FromRawFd, net::UnixStream},
    str::FromStr,
    sync::Arc,
};
use surf_n_term::{widgets::Theme, Key};
use sweep::{
    rpc_call, rpc_decode, rpc_encode, Candidate, FieldSelector, FuzzyScorer, Scorer, ScorerBuilder,
    SubstrScorer, Sweep, SweepEvent, SweepOptions,
};

const SCORER_NEXT_TAG: &str = "scorer_next";

fn main() -> Result<(), Error> {
    let mut args: Args = argh::from_env();

    if args.version {
        println!(
            "sweep {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("COMMIT_INFO")
        );
        return Ok(());
    }

    let (input, mut output): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match args.io_socket {
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
            (Box::new(std::io::stdin()), Box::new(std::io::stdout()))
        }
        Some(ref address) => {
            let input = match address.parse() {
                Ok(fd) => unsafe { UnixStream::from_raw_fd(fd) },
                Err(_) => {
                    UnixStream::connect(&address).context("failed to connnect to io-socket")?
                }
            };
            let output = input.try_clone().context("failed to duplicate io-socket")?;
            (Box::new(input), Box::new(output))
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
    sweep.bind(Key::chord("ctrl+s")?, SCORER_NEXT_TAG.into());

    if !args.rpc {
        let (haystack_send, haystack_recv) = unbounded();
        if args.json {
            let request = serde_json::from_reader(input).context("failed to parse input JSON")?;
            let items = match request {
                Value::Array(items) => items,
                _ => return Err(anyhow!("input must be an array")),
            };
            let candidates = items
                .into_iter()
                .map(|item| {
                    Candidate::from_json(item, args.field_delimiter, args.field_selector.as_ref())
                        .context("failed to parse input entry")
                })
                .collect::<Result<_, _>>()?;
            haystack_send.send(candidates)?;
        } else {
            Candidate::load_from_reader(
                input,
                args.field_delimiter,
                args.field_selector.clone(),
                move |haystack| {
                    let _ = haystack_send.send(haystack);
                },
            );
        }
        let events = sweep.events();
        let mut haystack_recv = Some(&haystack_recv);
        loop {
            select! {
                recv(haystack_recv.unwrap_or(&never())) -> haystack => {
                    match haystack {
                        Ok(haystack) => sweep.haystack_extend(haystack),
                        Err(_) => {
                            haystack_recv.take();
                        }
                    }
                }
                recv(events) -> event => {
                    match event {
                        Ok(SweepEvent::Select(result)) => {
                            if result.is_none() && !args.no_match_use_input {
                                continue
                            }
                            let input = sweep.niddle_get()?;
                            std::mem::drop(sweep);
                            if args.json {
                                let result = result.map_or_else(|| input.into(), |value| value.to_json());
                                serde_json::to_writer(output, &result)?;
                            } else {
                                writeln!(output, "{}", result.map_or_else(|| input, |value| value.to_string()))?;
                            }
                            break;
                        }
                        Ok(SweepEvent::Bind(tag)) => match tag {
                            Value::String(tag) if tag == SCORER_NEXT_TAG => {
                                sweep.scorer_set(args.scorer.toggle());
                            }
                            _ => {}
                        },
                        Err(_) => break,
                    }
                }
            }
        }
    } else {
        let rpc = rpc_decode(input, || true);
        let events = sweep.events();
        loop {
            select! {
                recv(rpc) -> request => {
                    let request = match request {
                        Ok(request) => request,
                        Err(_) => {
                            // RPC socket was closed
                            break
                        }
                    };
                    let request = match request {
                        Ok(request) => request,
                        Err(error) => {
                            rpc_encode(&mut output, error.into())?;
                            continue
                        }
                    };
                    let response = sweep.process_request(
                        request,
                        args.field_delimiter,
                        args.field_selector.as_ref()
                    );
                    if let Some(response) = response {
                        rpc_encode(&mut output, response)?;
                    }
                }
                recv(events) -> event => {
                    match event {
                        Ok(SweepEvent::Select(result)) => {
                            match result {
                                Some(result) => {
                                    rpc_call(&mut output, "select", result.to_json())?;
                                }
                                None => {
                                    if args.no_match_use_input {
                                        rpc_call(&mut output, "select", sweep.niddle_get()?)?;
                                    }
                                }
                            }
                        }
                        Ok(SweepEvent::Bind(tag)) => {
                            match tag {
                                Value::String(tag) if tag == SCORER_NEXT_TAG => {
                                    sweep.scorer_set(args.scorer.toggle());
                                }
                                _ => rpc_call(&mut output, "bind", tag)?,
                            }
                        }
                        Err(_) => break,
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
