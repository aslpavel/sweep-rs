#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

use anyhow::{anyhow, Context, Error};
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
    let mut args = Args::new()?;

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
        Some(address) => {
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
    })?;
    sweep.bind(Key::chord("ctrl+s")?, SCORER_NEXT_TAG.into());

    if !args.rpc {
        let (haystack_send, haystack_recv) = unbounded();
        if args.json {
            let request = serde_json::from_reader(input).context("failed to parse input JSON")?;
            let items = match request {
                Value::Array(items) => items,
                _ => return Err(anyhow!("JSON array expected as an input")),
            };
            let mut candidates = Vec::new();
            for item in items {
                let candidate = Candidate::from_json(
                    item.clone(),
                    args.field_delimiter,
                    args.field_selector.as_ref(),
                )
                .ok_or_else(|| anyhow!("Failed parse item as a candidate: {}", item))?;
                candidates.push(candidate);
            }
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

pub struct Args {
    pub height: usize,
    pub prompt: String,
    pub theme: Theme,
    pub field_selector: Option<FieldSelector>,
    pub field_delimiter: char,
    pub keep_order: bool,
    pub scorer: ScorerSelector,
    pub debug: bool,
    pub rpc: bool,
    pub tty_path: String,
    pub no_match_use_input: bool,
    pub title: String,
    pub altscreen: bool,
    pub json: bool,
    pub io_socket: Option<String>,
}

impl Args {
    pub fn new() -> Result<Self, Error> {
        use clap::{AppSettings, Arg};

        let matches = clap::App::new("sweep")
            .setting(AppSettings::ColoredHelp)
            .version(format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("COMMIT_INFO")).as_ref())
            .about("Sweep is a command line fuzzy finder")
            .author(env!("CARGO_PKG_AUTHORS"))
            .arg(
                Arg::with_name("prompt")
                    .short("p")
                    .long("prompt")
                    .takes_value(true)
                    .help("prompt string"),
            )
            .arg(
                Arg::with_name("height")
                    .long("height")
                    .takes_value(true)
                    .help("height occupied by the sweep list"),
            )
            .arg(
                Arg::with_name("theme")
                    .long("theme")
                    .takes_value(true)
                    .help("specify theme as a list of comma sperated attributes"),
            )
            .arg(
                Arg::with_name("field_selector")
                    .long("nth")
                    .takes_value(true)
                    .help("comma-separated list of fields for limiting search scope"),
            )
            .arg(
                Arg::with_name("keep_order")
                    .long("keep-order")
                    .help("keep order (don't use ranking score)"),
            )
            .arg(
                Arg::with_name("scorer")
                    .long("scorer")
                    .takes_value(true)
                    .possible_values(&["fuzzy", "substr"])
                    .help("default scorer to rank candidates"),
            )
            .arg(
                Arg::with_name("debug")
                    .long("debug")
                    .help("enabled debugging output"),
            )
            .arg(
                Arg::with_name("delimiter")
                    .long("delimiter")
                    .short("d")
                    .takes_value(true)
                    .help("field delimiter"),
            )
            .arg(
                Arg::with_name("rpc")
                    .long("rpc")
                    .help("use JSON-RPC protocol to communicate"),
            )
            .arg(
                Arg::with_name("io-socket")
                    .long("io-socket")
                    .takes_value(true)
                    .help(
                        "path/descriptor of the unix socket used to communicate instead of stdio/stdin",
                    ),
            )
            .arg(
                Arg::with_name("tty")
                    .long("tty")
                    .default_value("/dev/tty")
                    .help("path to the tty"),
            )
            .arg(
                Arg::with_name("no-match")
                    .long("no-match")
                    .takes_value(true)
                    .default_value("nothing")
                    .possible_values(&["nothing", "input"])
                    .help("string returned if there is no match"),
            )
            .arg(
                Arg::with_name("title")
                    .long("title")
                    .takes_value(true)
                    .default_value("sweep")
                    .help("set terminal title"),
            )
            .arg(
                Arg::with_name("altscreen")
                    .long("altscreen")
                    .help("use alternative screen"),
            )
            .arg(
                Arg::with_name("json")
                    .long("json")
                    .help("expect candidates in JOSN format"),
            )
            .get_matches();

        let prompt = match matches.value_of("prompt") {
            Some(prompt) => prompt.to_string(),
            None => "INPUT".to_string(),
        };

        let height = matches
            .value_of("height")
            .map(|h| h.parse::<usize>())
            .transpose()?
            .unwrap_or(11);

        let theme = match matches.value_of("theme") {
            Some(theme) => theme.parse()?,
            None => Theme::light(),
        };

        let field_selector = matches
            .value_of("field_selector")
            .map(|h| h.parse())
            .transpose()?;

        let keep_order = matches.is_present("keep_order");

        let scorer = matches.value_of("scorer").unwrap_or("fuzzy").parse()?;

        let debug = matches.is_present("debug");

        let field_delimiter = match matches.value_of("delimiter") {
            None => ' ',
            Some(delimiter) => delimiter.parse()?,
        };

        let rpc = matches.is_present("rpc");
        let io_socket = matches.value_of("io-socket").map(ToString::to_string);

        let tty_path = match matches.value_of("tty") {
            None => "/dev/tty".to_string(),
            Some(tty) => tty.to_string(),
        };

        let no_match_use_input = matches!(matches.value_of("no-match"), Some("input"));

        let title = matches.value_of("title").unwrap_or("sweep").to_string();

        Ok(Self {
            prompt,
            height,
            theme,
            field_selector,
            field_delimiter,
            keep_order,
            scorer,
            debug,
            rpc,
            io_socket,
            tty_path,
            no_match_use_input,
            title,
            altscreen: matches.is_present("altscreen"),
            json: matches.is_present("json"),
        })
    }
}
