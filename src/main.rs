#![deny(warnings)]

use anyhow::{anyhow, Error};
use serde_json::json;
use std::{
    fmt,
    io::Write,
    os::unix::io::AsRawFd,
    sync::{mpsc::TryRecvError, Arc},
    time::{Duration, Instant},
};
use surf_n_term::{
    render::{TerminalWritable, TerminalWriter},
    widgets::{Input, List, ListItems, Theme},
    Blend, Color, DecMode, Face, FaceAttrs, Key, KeyMap, KeyMod, KeyName, Position, Surface,
    SurfaceMut, SystemTerminal, Terminal, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalSurfaceExt,
};
use sweep_lib::{
    rpc_encode, rpc_requests, Candidate, FieldSelector, FuzzyScorer, Haystack, RPCRequest, Ranker,
    RankerResult, ScoreResult, Scorer, SubstrScorer,
};

fn main() -> Result<(), Error> {
    let mut args = Args::new()?;
    let theme = args.theme.clone();

    if nix::unistd::isatty(std::io::stdin().as_raw_fd())? {
        return Err(anyhow!("stdin can not be a tty, pipe in data instead"));
    }
    if args.rpc && nix::unistd::isatty(std::io::stdout().as_raw_fd())? {
        return Err(anyhow!("stdout can not be a tty if rpc is enabled"));
    }

    let debug_face: Face = "bg=#cc241d,fg=#ebdbb2".parse()?;
    let stats_face = Face::new(
        Some(theme.accent.best_contrast(theme.bg, theme.fg)),
        Some(theme.accent),
        FaceAttrs::EMPTY,
    );
    let label_face = stats_face.with_attrs(FaceAttrs::BOLD);
    let separator_face = Face::new(Some(theme.accent), theme.input.bg, FaceAttrs::EMPTY);

    // size
    let height = args.height;

    // initialize terminal
    let mut term = SystemTerminal::open(&args.tty_path)?;
    // term.duplicate_output("/tmp/sweep.log")?;
    term.execute(TerminalCommand::DecModeSet {
        enable: false,
        mode: DecMode::VisibleCursor,
    })?;

    // find current row offset
    let mut row_offset = 0;
    term.execute(TerminalCommand::CursorGet)?;
    while let Some(event) = term.poll(None)? {
        if let TerminalEvent::CursorPosition { row, .. } = event {
            row_offset = row;
            break;
        }
    }
    let term_size = term.size()?;
    if height > term_size.height {
        row_offset = 0;
    } else if row_offset + height > term_size.height {
        let scroll = row_offset + height - term_size.height;
        row_offset = term_size.height - height;
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }

    // initialize ranker
    let waker = term.waker();
    let ranker = Ranker::new(args.scorer.next(), args.keep_order, move || {
        waker.wake().is_ok()
    });
    let requests = if !args.rpc {
        Candidate::load_stdin(
            ranker.clone(),
            args.field_delimiter,
            args.field_selector.clone(),
            args.reversed,
        );
        if args.reversed {
            ranker.haystack_reverse();
        }
        None
    } else {
        let waker = term.waker();
        Some(rpc_requests(std::io::stdin(), move || waker.wake().is_ok()))
    };

    // initialize widgets
    let mut prompt = args.prompt.clone();
    let mut input = Input::new();
    let mut list = List::new(RankerResultThemed::new(
        theme.clone(),
        Arc::new(RankerResult::<Candidate>::default()),
    ));

    // rpc key bindings
    let mut key_map = KeyMap::new();
    let mut key_map_state = Vec::new();

    // render loop
    let result = term.run_render(|term, event, view| -> Result<_, Error> {
        let frame_start = Instant::now();

        // handle events
        if let Some(event) = &event {
            match *event {
                TerminalEvent::Key(Key { name, mode }) if mode == KeyMod::CTRL => {
                    if name == KeyName::Char('c') {
                        return Ok(TerminalAction::Quit(None));
                    } else if name == KeyName::Char('m') || name == KeyName::Char('j') {
                        let result = match list.current() {
                            Some(candidate) => Some(candidate.result.haystack.to_string()),
                            None if args.no_match_use_input => {
                                Some(input.get().collect::<String>())
                            }
                            _ => None,
                        };
                        if let Some(result) = result {
                            if args.rpc {
                                rpc_encode(std::io::stdout(), json!({ "selected": result }))?;
                            } else {
                                return Ok(TerminalAction::Quit(Some(result)));
                            }
                        }
                    } else if name == KeyName::Char('s') {
                        ranker.scorer_set(args.scorer.next());
                    }
                }
                TerminalEvent::Resize(term_size) => {
                    if height > term_size.height {
                        row_offset = 0;
                    } else if row_offset + height > term_size.height {
                        row_offset = term_size.height - height;
                    }
                }
                TerminalEvent::Wake => {
                    // handle rpc requests
                    if let Some(requests) = requests.as_ref() {
                        loop {
                            let request = match requests.try_recv() {
                                Ok(request) => request,
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    return Ok(TerminalAction::Quit(None))
                                }
                            };
                            use RPCRequest::*;
                            match request {
                                Ok(PromptSet(new_prompt)) => prompt = new_prompt,
                                Ok(KeyBinding { key, tag }) => key_map.register(key.as_ref(), tag),
                                Ok(NiddleSet(niddle)) => {
                                    input.set(niddle.as_ref());
                                }
                                Ok(CandidatesExtend { items }) => {
                                    let items = items
                                        .into_iter()
                                        .map(|c| {
                                            Candidate::new(
                                                c,
                                                args.field_delimiter,
                                                &args.field_selector,
                                            )
                                        })
                                        .collect();
                                    ranker.haystack_extend(items);
                                }
                                Ok(CandidatesClear) => ranker.haystack_clear(),
                                Ok(Terminate) => return Ok(TerminalAction::Quit(None)),
                                Ok(Current) => {
                                    let result = match list.current() {
                                        Some(candidate) => {
                                            Some(candidate.result.haystack.to_string())
                                        }
                                        None if args.no_match_use_input => {
                                            Some(input.get().collect::<String>())
                                        }
                                        _ => None,
                                    };
                                    rpc_encode(std::io::stdout(), json!({ "current": result }))?;
                                }
                                Err(msg) => rpc_encode(std::io::stdout(), json!({ "error": msg }))?,
                            }
                        }
                    }
                }
                _ => (),
            }
            match *event {
                TerminalEvent::Key(key) => {
                    if let Some(tag) = key_map.lookup_state(&mut key_map_state, key) {
                        if key != Key::new(KeyName::Backspace, KeyMod::EMPTY)
                            || input.get().count() == 0
                        {
                            rpc_encode(std::io::stdout(), json!({ "key_binding": tag.clone() }))?
                        }
                    }
                }
                _ => (),
            }
            input.handle(event);
            list.handle(event);
        }
        // restrict view
        let mut view = view.view_owned((row_offset as i32).., 1..-1);

        // update niddle
        ranker.niddle_set(input.get().collect());
        let ranker_result = ranker.result();

        // label
        let mut label_view = view.view_mut(0, ..);
        let mut label = label_view.writer().face(label_face);
        write!(&mut label, " {} ", prompt)?;
        let mut label = label.face(separator_face);
        write!(&mut label, " ")?;
        let input_start = label.position().1 as i32;

        // stats
        let stats_str = format!(
            " {}/{} {:.2?} [{}] ",
            ranker_result.result.len(),
            ranker_result.haystack_size,
            ranker_result.duration,
            ranker_result.scorer.name(),
        );
        let input_stop = -(stats_str.chars().count() as i32 + 1);
        let mut stats_view = view.view_mut(0, input_stop..);
        let mut stats = stats_view.writer().face(separator_face);
        write!(&mut stats, "")?;
        let mut stats = stats.face(stats_face);
        stats.write_all(stats_str.as_ref())?;

        // input
        input.render(&theme, view.view_mut(0, input_start..input_stop))?;

        // list
        if list.items().generation() != ranker_result.generation {
            let old_result = list.items_set(RankerResultThemed::new(theme.clone(), ranker_result));
            // dropping old result might add noticeable delay for large lists
            rayon::spawn(move || std::mem::drop(old_result));
        }
        list.render(&theme, view.view_mut(1..height as i32, ..))?;

        if args.debug {
            let frame_time = Instant::now() - frame_start;
            let debug_height = (height as i32 + 1)..(height as i32 + 7);
            let mut debug = view.view_mut(debug_height, ..);
            debug.erase(debug_face.bg);
            let mut debug_writer = debug.writer().face(debug_face);
            writeln!(&mut debug_writer, "row: {}", row_offset)?;
            writeln!(&mut debug_writer, "frame_time: {:?}", frame_time)?;
            writeln!(&mut debug_writer, "event: {:?}", event)?;
            writeln!(&mut debug_writer, "term: {:?}", term.stats())?;
            writeln!(
                &mut debug_writer,
                "current: {:?}",
                list.current().map(|r| r.result)
            )?;
        }

        Ok(TerminalAction::Wait)
    });

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: row_offset,
        col: 0,
    }))?;
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    // print result
    match result {
        Ok(result) => {
            if let Some(result) = result {
                println!("{}", result.to_string());
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone)]
pub struct ScorerSelector {
    scorers: Vec<Arc<dyn Scorer>>,
    index: usize,
}

impl Default for ScorerSelector {
    fn default() -> Self {
        Self::new(vec![
            Arc::new(FuzzyScorer::new()),
            Arc::new(SubstrScorer::new()),
        ])
    }
}

impl ScorerSelector {
    pub fn new(scorers: Vec<Arc<dyn Scorer>>) -> Self {
        if scorers.is_empty() {
            Default::default()
        } else {
            Self { scorers, index: 0 }
        }
    }

    pub fn from_str(name: &str) -> Result<Self, Error> {
        let this = Self::default();
        let index = this
            .scorers
            .iter()
            .enumerate()
            .find_map(|(i, s)| if s.name() == name { Some(i) } else { None })
            .ok_or_else(|| anyhow!("Unknown scorer: {}", name))?;
        Ok(Self { index, ..this })
    }

    pub fn current(&self) -> &Arc<dyn Scorer> {
        &self.scorers[self.index]
    }

    pub fn next(&mut self) -> Arc<dyn Scorer> {
        let scorer = self.scorers[self.index].clone();
        self.index = (self.index + 1) % self.scorers.len();
        scorer
    }
}

impl fmt::Debug for ScorerSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScorerSelector({})", self.current().name())
    }
}

pub struct Args {
    pub height: usize,
    pub prompt: String,
    pub theme: Theme,
    pub field_selector: Option<FieldSelector>,
    pub field_delimiter: char,
    pub keep_order: bool,
    pub reversed: bool,
    pub scorer: ScorerSelector,
    pub debug: bool,
    pub rpc: bool,
    pub tty_path: String,
    pub no_match_use_input: bool,
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
                Arg::with_name("reversed")
                    .short("r")
                    .long("reversed")
                    .help("reverse initial order of elements"),
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
                    .help("use JSON RPC protocol to communicate"),
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

        let reversed = matches.is_present("reversed");

        let scorer = ScorerSelector::from_str(matches.value_of("scorer").unwrap_or("fuzzy"))?;

        let debug = matches.is_present("debug");

        let field_delimiter = match matches.value_of("delimiter") {
            None => ' ',
            Some(delimiter) => delimiter.parse()?,
        };

        let rpc = matches.is_present("rpc");

        let tty_path = match matches.value_of("tty") {
            None => "/dev/tty".to_string(),
            Some(tty) => tty.to_string(),
        };

        let no_match_use_input = match matches.value_of("no-match") {
            Some("input") => true,
            _ => false,
        };

        Ok(Self {
            prompt,
            height,
            theme,
            field_selector,
            field_delimiter,
            keep_order,
            scorer,
            reversed,
            debug,
            rpc,
            tty_path,
            no_match_use_input,
        })
    }
}

#[derive(Debug)]
struct ScoreResultThemed<H> {
    result: ScoreResult<H>,
    face_default: Face,
    face_inactive: Face,
    face_highlight: Face,
}

impl<H: Haystack> TerminalWritable for ScoreResultThemed<H> {
    fn fmt(&self, writer: &mut TerminalWriter<'_>) -> std::io::Result<()> {
        let mut index = 0;
        for field in self.result.haystack.fields() {
            match field {
                Err(field) => {
                    writer.face_set(self.face_inactive);
                    writer.write_all(field.as_ref())?;
                    writer.face_set(self.face_default);
                }
                Ok(field) => {
                    for c in field.chars() {
                        if self.result.positions.contains(&index) {
                            writer.put_char(c, self.face_highlight);
                        } else {
                            writer.put_char(c, self.face_default);
                        }
                        index += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn height_hint(&self, width: usize) -> Option<usize> {
        let mut length = 0;
        for field in self.result.haystack.fields() {
            let field = match field {
                Ok(field) => field,
                Err(field) => field,
            };
            for c in field.chars() {
                length += match c {
                    '\n' => width - length % width,
                    _ => 1,
                }
            }
        }
        Some(length / width + (if length % width != 0 { 1 } else { 0 }))
    }
}

struct RankerResultThemed<H> {
    theme: Theme,
    ranker_result: Arc<RankerResult<H>>,
}

impl<H> RankerResultThemed<H> {
    fn new(theme: Theme, ranker_result: Arc<RankerResult<H>>) -> Self {
        Self {
            theme,
            ranker_result,
        }
    }

    fn generation(&self) -> usize {
        self.ranker_result.generation
    }
}

impl<H: Clone + Haystack> ListItems for RankerResultThemed<H> {
    type Item = ScoreResultThemed<H>;

    fn len(&self) -> usize {
        self.ranker_result.result.len()
    }

    fn get(&self, index: usize) -> Option<Self::Item> {
        let face_default = Face::default().with_fg(Some(self.theme.fg));
        let face_inactive = Face::default().with_fg(Some(
            self.theme
                .bg
                .blend(self.theme.fg.with_alpha(0.6), Blend::Over),
        ));
        self.ranker_result
            .result
            .get(index)
            .map(|result| ScoreResultThemed {
                result: result.clone(),
                face_default,
                face_inactive,
                face_highlight: self.theme.cursor,
            })
    }
}
