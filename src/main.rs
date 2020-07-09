#![deny(warnings)]

use anyhow::{anyhow, Error};
use std::{
    io::Write,
    os::unix::io::AsRawFd,
    sync::Arc,
    time::{Duration, Instant},
};
use surf_n_term::{
    render::{TerminalWritable, TerminalWriter},
    widgets::{Input, List, ListItems, Theme},
    Blend, Color, DecMode, Face, FaceAttrs, Key, KeyMod, KeyName, Position, Surface, SurfaceMut,
    SystemTerminal, Terminal, TerminalAction, TerminalCommand, TerminalEvent, TerminalSurfaceExt,
};

mod score;
use score::{FuzzyScorer, Haystack, ScoreResult, Scorer, SubstrScorer};
mod rank;
use rank::{Ranker, RankerResult};
mod candidate;
use candidate::{Candidate, FieldSelector};

fn main() -> Result<(), Error> {
    if nix::unistd::isatty(std::io::stdin().as_raw_fd())? {
        return Err(anyhow!("stdin can not be a tty, pipe in data instead"));
    }

    let mut args = Args::new()?;
    let theme = args.theme.clone();

    let debug_face: Face = "bg=#cc241d,fg=#ebdbb2".parse()?;
    let stats_face = Face::new(
        Some(theme.accent.best_contrast(theme.bg, theme.fg)),
        Some(theme.accent),
        FaceAttrs::EMPTY,
    );
    let label_face = stats_face.with_attrs(FaceAttrs::BOLD);
    let separator_face = Face::new(Some(theme.accent), theme.input.bg, FaceAttrs::EMPTY);

    // size
    let height_u = args.height;
    let height = args.height as i32;

    // initialize terminal
    let mut term = SystemTerminal::new()?;
    // term.duplicate_output("/tmp/sweep.log")?;
    term.execute(TerminalCommand::DecModeSet {
        enable: false,
        mode: DecMode::VisibleCursor,
    })?;

    // find current row offset
    let mut row_offset = 0;
    term.execute(TerminalCommand::CursorGet)?;
    match term.poll(Some(Duration::from_millis(500)))? {
        Some(TerminalEvent::CursorPosition { row, .. }) => {
            row_offset = row;
        }
        _ => (),
    }
    let term_size = term.size()?;
    if height_u > term_size.height {
        row_offset = 0;
    } else if row_offset + height_u > term_size.height {
        let scroll = row_offset + height_u - term_size.height;
        row_offset = term_size.height - height_u;
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }

    // initialize ranker
    let waker = term.waker();
    let ranker = Ranker::new(args.scorer.next(), args.keep_order, move || {
        waker.wake().is_ok()
    });
    Candidate::load_stdin(ranker.clone(), args.delimiter, args.field_selector.clone());

    // initialize widgets
    let mut input = Input::new();
    let mut list = List::new(RankerResultThemed::new(
        theme.clone(),
        Arc::new(RankerResult::<Candidate>::default()),
    ));

    // render loop
    let mut result = None;
    term.run_render(|term, event, view| -> Result<_, Error> {
        let frame_start = Instant::now();

        // handle events
        if let Some(event) = &event {
            match *event {
                TerminalEvent::Key(Key { name, mode }) if mode == KeyMod::CTRL => {
                    if name == KeyName::Char('c') {
                        return Ok(TerminalAction::Quit);
                    } else if name == KeyName::Char('m') || name == KeyName::Char('j') {
                        if let Some(candidate) = list.current() {
                            result.replace(candidate.result.haystack);
                            return Ok(TerminalAction::Quit);
                        }
                    } else if name == KeyName::Char('s') {
                        ranker.scorer_set(args.scorer.next());
                    }
                }
                TerminalEvent::Resize(term_size) => {
                    if height_u > term_size.height {
                        row_offset = 0;
                    } else if row_offset + height_u > term_size.height {
                        row_offset = term_size.height - height_u;
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
        let result = ranker.result();

        // label
        let mut label_view = view.view_mut(0, ..);
        let mut label = label_view.writer().face(label_face);
        write!(&mut label, " {} ", args.prompt)?;
        let mut label = label.face(separator_face);
        write!(&mut label, " ")?;
        let input_start = label.position().1 as i32;

        // stats
        let stats_str = format!(
            " {}/{} {:.2?} [{}] ",
            result.result.len(),
            result.haystack_size,
            result.duration,
            result.scorer.name(),
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
        if list.items().result.generation != result.generation {
            list.items_set(RankerResultThemed::new(theme.clone(), result));
        }
        list.render(&theme, view.view_mut(1..height, ..))?;

        if args.debug {
            let frame_duration = Instant::now() - frame_start;
            let mut debug = view.view_mut((height + 1)..(height + 7), ..);
            debug.erase(debug_face.bg);
            let mut debug_writer = debug.writer().face(debug_face);
            writeln!(&mut debug_writer, "row: {}", row_offset)?;
            writeln!(&mut debug_writer, "frame_time: {:.2?}", frame_duration)?;
            writeln!(&mut debug_writer, "event: {:?}", event)?;
            writeln!(&mut debug_writer, "term: {:?}", term.stats())?;
            writeln!(
                &mut debug_writer,
                "current: {:?}",
                list.current().map(|r| r.result)
            )?;
        }

        Ok(TerminalAction::Wait)
    })?;

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: row_offset,
        col: 0,
    }))?;
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    // print result
    if let Some(result) = result {
        println!("{}", result.to_string());
    }

    Ok(())
}

pub struct ScorerSelector {
    scorers: Vec<Arc<dyn Scorer>>,
    index: usize,
}

impl ScorerSelector {
    pub fn new(name: &str) -> Result<Self, Error> {
        let scorers: Vec<Arc<dyn Scorer>> =
            vec![Arc::new(FuzzyScorer::new()), Arc::new(SubstrScorer::new())];
        let index = scorers
            .iter()
            .enumerate()
            .find_map(|(i, s)| if s.name() == name { Some(i) } else { None })
            .ok_or_else(|| anyhow!("Unknown scorer: {}", name))?;
        Ok(Self { scorers, index })
    }

    pub fn next(&mut self) -> Arc<dyn Scorer> {
        let scorer = self.scorers[self.index].clone();
        self.index = (self.index + 1) % self.scorers.len();
        scorer
    }
}

pub struct Args {
    pub height: usize,
    pub prompt: String,
    pub theme: Theme,
    pub field_selector: Option<FieldSelector>,
    pub keep_order: bool,
    pub reversed: bool,
    pub scorer: ScorerSelector,
    pub debug: bool,
    pub delimiter: char,
}

impl Args {
    pub fn new() -> Result<Self, Error> {
        use clap::Arg;

        let matches = clap::App::new("sweep")
            .version(env!("CARGO_PKG_VERSION"))
            .about("Sweep is a command line fuzzy finder")
            .author("Pavel Aslanov")
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

        let scorer = ScorerSelector::new(matches.value_of("scorer").unwrap_or("fuzzy"))?;

        let debug = matches.is_present("debug");

        let delimiter = match matches.value_of("delimiter") {
            None => ' ',
            Some(delimiter) => delimiter.parse()?,
        };

        Ok(Self {
            prompt,
            height,
            theme,
            field_selector,
            keep_order,
            scorer,
            reversed,
            debug,
            delimiter,
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
}

struct RankerResultThemed<H> {
    theme: Theme,
    result: Arc<RankerResult<H>>,
}

impl<H> RankerResultThemed<H> {
    fn new(theme: Theme, result: Arc<RankerResult<H>>) -> Self {
        Self { theme, result }
    }
}

impl<H: Clone + Haystack> ListItems for RankerResultThemed<H> {
    type Item = ScoreResultThemed<H>;
    fn len(&self) -> usize {
        self.result.result.len()
    }
    fn get(&self, index: usize) -> Option<Self::Item> {
        let face_default = Face::default().with_fg(Some(self.theme.fg));
        let face_inactive = Face::default().with_fg(Some(
            self.theme
                .bg
                .blend(self.theme.fg.with_alpha(0.6), Blend::Over),
        ));
        self.result
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
