use crate::{
    Candidate, FieldSelector, FuzzyScorer, Haystack, RPCErrorKind, RPCRequest, Ranker,
    RankerResult, ScoreResult, ScorerBuilder,
};
use anyhow::{Context, Error};
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    io::Write,
    ops::Deref,
    sync::Arc,
    thread::{Builder, JoinHandle},
    time::Duration,
};
use surf_n_term::{
    widgets::{Input, InputAction, List, ListAction, ListItems, Theme},
    Blend, Cell, Color, DecMode, Face, FaceAttrs, Key, KeyMap, KeyMod, KeyName, Position, Surface,
    SurfaceMut, SystemTerminal, Terminal, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalSurfaceExt, TerminalWaker, TerminalWritable, TerminalWriter,
};

pub const SCORER_NEXT_TAG: &str = "sweep.scorer.next";

pub struct SweepOptions {
    pub height: usize,
    pub prompt: String,
    pub theme: Theme,
    pub keep_order: bool,
    pub tty_path: String,
    pub title: String,
    pub scorer_builder: ScorerBuilder,
    pub altscreen: bool,
    pub debug: bool,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            height: 11,
            prompt: "INPUT".to_string(),
            theme: Theme::light(),
            keep_order: false,
            tty_path: "/dev/tty".to_string(),
            title: "sweep".to_string(),
            scorer_builder: Arc::new(|niddle: &str| {
                let niddle: Vec<_> = niddle.chars().flat_map(char::to_lowercase).collect();
                Arc::new(FuzzyScorer::new(niddle))
            }),
            altscreen: false,
            debug: false,
        }
    }
}

/// Simple sweep function when you just need to select single entry from the list
pub fn sweep<H, HS>(options: SweepOptions, haystack: HS) -> Result<Option<H>, Error>
where
    HS: IntoIterator,
    H: Haystack + From<HS::Item>,
{
    let sweep = Sweep::new(options)?;
    sweep.haystack_extend(haystack);
    for event in sweep.events().iter() {
        if let SweepEvent::Select(Some(entry)) = event {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

enum SweepCommand {
    NiddleSet(String),
    NiddleGet,
    PromptSet(String),
    Bind(Vec<Key>, String),
    Terminate,
    Current,
}
#[derive(Clone, Debug)]
pub enum SweepEvent<H> {
    Select(Option<H>),
    Bind(String),
}

#[derive(Clone, Debug)]
pub enum SweepResponse<H> {
    Current(Option<H>),
    Niddle(String),
}

#[derive(Clone)]
pub struct Sweep<H>
where
    H: Haystack,
{
    inner: Arc<SweepInner<H>>,
}

impl<H: Haystack> Deref for Sweep<H> {
    type Target = SweepInner<H>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<H> Sweep<H>
where
    H: Haystack,
{
    pub fn new(options: SweepOptions) -> Result<Self, Error> {
        let inner = Arc::new(SweepInner::new(options)?);
        Ok(Sweep { inner })
    }

    fn send_command(&self, command: SweepCommand) {
        self.commands
            .send(command)
            .expect("failed to send command to sweep_worker");
        self.waker.wake().expect("failed to wake terminal");
    }

    /// Extend haystack with entries
    pub fn haystack_extend<HS>(&self, haystack: HS)
    where
        HS: IntoIterator,
        H: From<HS::Item>,
    {
        self.ranker
            .haystack_extend(haystack.into_iter().map(From::from).collect())
    }

    /// Remove all entries from the haystack
    pub fn haystack_clear(&self) {
        self.ranker.haystack_clear()
    }

    /// Reverse haystack
    pub fn haystack_reverse(&self) {
        self.ranker.haystack_reverse()
    }

    /// Set niddle to the spcified string
    pub fn niddle_set(&self, niddle: impl AsRef<str>) {
        self.send_command(SweepCommand::NiddleSet(niddle.as_ref().to_string()))
    }

    /// Get current niddle value
    pub fn niddle_get(&self) -> Result<String, Error> {
        self.send_command(SweepCommand::NiddleGet);
        loop {
            if let SweepResponse::Niddle(niddle) = self.response.recv()? {
                return Ok(niddle);
            }
        }
    }

    /// Set scorer used for ranking
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.ranker.scorer_set(scorer)
    }

    /// Set prompt
    pub fn prompt_set(&self, prompt: String) {
        self.send_command(SweepCommand::PromptSet(prompt))
    }

    /// Get currently selected candidates
    pub fn current(&self) -> Result<Option<H>, Error> {
        self.send_command(SweepCommand::Current);
        loop {
            if let SweepResponse::Current(current) = self.response.recv()? {
                return Ok(current);
            }
        }
    }

    /// Bind specified chord to the tag
    ///
    /// Whenever sequence of keys specified by chord is pressed, `SweepEvent::Bind(tag)`
    /// will be generated, note if tag is empty string the binding will be removed
    /// and no event will be generated. Tag can also be one of the standard actions
    /// list of which is available with `ctrl+h`
    pub fn bind(&self, chord: Vec<Key>, tag: String) {
        self.send_command(SweepCommand::Bind(chord, tag))
    }

    /// Event generated by the `Sweep` object
    pub fn events(&self) -> &Receiver<SweepEvent<H>> {
        &self.events
    }
}

pub struct SweepInner<H: Haystack> {
    ranker: Ranker<H>,
    waker: TerminalWaker,
    worker: Option<JoinHandle<Result<(), Error>>>,
    commands: Sender<SweepCommand>,
    response: Receiver<SweepResponse<H>>,
    events: Receiver<SweepEvent<H>>,
}

impl<H: Haystack> SweepInner<H> {
    pub fn new(options: SweepOptions) -> Result<Self, Error> {
        let (commands_send, commands_recv) = unbounded();
        let (events_send, events_recv) = unbounded();
        let (response_send, response_recv) = unbounded();
        let term = SystemTerminal::open(&options.tty_path)
            .with_context(|| format!("failed to open terminal: {}", options.tty_path))?;
        let waker = term.waker();
        let ranker = Ranker::new(options.scorer_builder.clone(), options.keep_order, {
            let waker = waker.clone();
            move || waker.wake().is_ok()
        });
        let worker = Builder::new().name("sweep-ui".to_string()).spawn({
            let ranker = ranker.clone();
            move || {
                sweep_worker(
                    options,
                    term,
                    ranker,
                    commands_recv,
                    response_send,
                    events_send,
                )
            }
        })?;
        Ok(SweepInner {
            ranker,
            waker,
            worker: Some(worker),
            commands: commands_send,
            response: response_recv,
            events: events_recv,
        })
    }
}

impl<H> Drop for SweepInner<H>
where
    H: Haystack,
{
    fn drop(&mut self) {
        self.commands.send(SweepCommand::Terminate).unwrap_or(());
        self.waker.wake().unwrap_or(());
        if let Some(handle) = self.worker.take() {
            if let Err(error) = handle.join() {
                eprintln!("sweep worker thread fail:\r\n{:?}", error);
            }
        }
    }
}

impl Sweep<Candidate> {
    pub fn process_request(
        &self,
        mut request: RPCRequest,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
    ) -> Option<Value> {
        let params = request.params.take();
        let result = match request.method.as_ref() {
            "haystack_extend" => {
                let items = if let Value::Array(items) = params {
                    items
                } else {
                    let error = request.response_err(
                        RPCErrorKind::InvalidParams,
                        Some("[haystack_extend] parameters must be an array"),
                    );
                    return Some(error);
                };
                let mut candidates = Vec::new();
                for item in items {
                    match Candidate::from_json(item, delimiter, field_selector) {
                        Ok(candidate) => candidates.push(candidate),
                        Err(error) => {
                            let error = request.response_err(
                                RPCErrorKind::InvalidParams,
                                Some(format!("[haystack_extend] {}", error)),
                            );
                            return Some(error);
                        }
                    }
                }
                self.haystack_extend(candidates);
                Value::Null
            }
            "haystack_clear" => {
                self.haystack_clear();
                Value::Null
            }
            "niddle_set" => {
                if let Value::String(niddle) = params {
                    self.niddle_set(niddle);
                    Value::Null
                } else {
                    let error = request.response_err(
                        RPCErrorKind::InvalidParams,
                        Some("[niddle_set] parameters must be a string"),
                    );
                    return Some(error);
                }
            }
            "niddle_get" => match self.niddle_get() {
                Ok(niddle) => niddle.into(),
                Err(error) => {
                    let error =
                        request.response_err(RPCErrorKind::InternalError, Some(error.to_string()));
                    return Some(error);
                }
            },
            "terminate" => {
                self.send_command(SweepCommand::Terminate);
                Value::Null
            }
            "key_binding" => {
                let mut obj = if let Value::Object(obj) = params {
                    obj
                } else {
                    let error = request.response_err(
                        RPCErrorKind::InvalidParams,
                        Some("[key_binding] parameters must be an object"),
                    );
                    return Some(error);
                };
                let key = if let Some(Value::String(key)) = obj.get_mut("key") {
                    match Key::chord(key) {
                        Ok(key) => key,
                        Err(error) => {
                            let error = request.response_err(
                                RPCErrorKind::InvalidParams,
                                Some(format!(
                                    "[key_binding] failed to parse key attribute: {}",
                                    error
                                )),
                            );
                            return Some(error);
                        }
                    }
                } else {
                    let error = request.response_err(
                        RPCErrorKind::InvalidParams,
                        Some("[key_binding] \"key\" attribute must be present and be a string"),
                    );
                    return Some(error);
                };
                let tag = match obj.get_mut("tag").and_then(|v| v.as_str()) {
                    Some(tag) => tag.to_owned(),
                    None => {
                        let error = request.response_err(
                            RPCErrorKind::InvalidParams,
                            Some("[key_binding] \"tag\" attribute must be present"),
                        );
                        return Some(error);
                    }
                };
                self.bind(key, tag);
                Value::Null
            }
            "prompt_set" => {
                if let Value::String(prompt) = params {
                    self.prompt_set(prompt);
                    Value::Null
                } else {
                    let error = request.response_err(
                        RPCErrorKind::InvalidParams,
                        Some("[prompt_set] parameters must be a string"),
                    );
                    return Some(error);
                }
            }
            "current" => match self.current() {
                Ok(current) => current.map_or_else(|| Value::Null, |current| current.to_json()),
                Err(error) => {
                    let error =
                        request.response_err(RPCErrorKind::InternalError, Some(error.to_string()));
                    return Some(error);
                }
            },
            method => {
                let error_data = Some(format!("unknown method: {}", method));
                let error = request.response_err(RPCErrorKind::MethodNotFound, error_data);
                return Some(error);
            }
        };
        request.response_ok(result)
    }
}

/// User bindable actions
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SweepAction {
    User(String),
    Select,
    Quit,
    Help,
    Input(InputAction),
    List(ListAction),
}

/// Object representing current state of the sweep worker
struct SweepState<H> {
    // sweep prompt
    prompt: String,
    // current state of the key chrod
    key_map_state: Vec<Key>,
    // user action executed on backspace when input is empty
    key_empty_backspace: Option<String>,
    // action key map
    key_map: KeyMap<SweepAction>,
    // action name to sweep action
    key_actions: HashMap<&'static str, SweepAction>,
    // theme
    theme: Theme,
    // face used for label (FIXME: merge into theme?)
    label_face: Face,
    // face used for separator
    separator_face: Face,
    // face sue for stats
    stats_face: Face,
    // input widget
    input: Input,
    // list widget
    list: List<RankerResultThemed<H>>,
    // ranker
    ranker: Ranker<H>,
}

/// Event generated by key handling
enum SweepKeyEvent<H> {
    Event(SweepEvent<H>),
    Help,
    Quit,
    Nothing,
}

impl<H> SweepState<H>
where
    H: Haystack,
{
    fn new(prompt: String, ranker: Ranker<H>, theme: Theme) -> Self {
        // faces
        let stats_face = Face::new(
            Some(theme.accent.best_contrast(theme.bg, theme.fg)),
            Some(theme.accent),
            FaceAttrs::EMPTY,
        );
        let label_face = stats_face.with_attrs(FaceAttrs::BOLD);
        let separator_face = Face::new(Some(theme.accent), theme.input.bg, FaceAttrs::EMPTY);

        // key map
        let mut key_map = KeyMap::new();
        let mut key_actions = HashMap::new();
        // input
        for desc in InputAction::description() {
            let action = SweepAction::Input(desc.action);
            key_actions.insert(desc.name, action.clone());
            for chord in desc.chord {
                key_map.register(chord, action.clone());
            }
        }
        // list
        for desc in ListAction::description() {
            let action = SweepAction::List(desc.action);
            key_actions.insert(desc.name, action.clone());
            for chord in desc.chord {
                key_map.register(chord, action.clone());
            }
        }
        // help
        key_actions.insert("sweep.help", SweepAction::Help);
        key_map.register(
            &[Key {
                name: KeyName::Char('h'),
                mode: KeyMod::CTRL,
            }],
            SweepAction::Help,
        );
        // next scorer
        let scorer_next = SweepAction::User(SCORER_NEXT_TAG.to_owned());
        key_actions.insert(SCORER_NEXT_TAG, scorer_next.clone());
        key_map.register(
            &[Key {
                name: KeyName::Char('s'),
                mode: KeyMod::CTRL,
            }],
            scorer_next,
        );
        // quit
        key_actions.insert("sweep.quit", SweepAction::Quit);
        key_map.register(
            &[Key {
                name: KeyName::Char('c'),
                mode: KeyMod::CTRL,
            }],
            SweepAction::Quit,
        );
        key_map.register(
            &[Key {
                name: KeyName::Esc,
                mode: KeyMod::EMPTY,
            }],
            SweepAction::Quit,
        );
        // select
        key_actions.insert("sweep.select", SweepAction::Select);
        key_map.register(
            &[Key {
                name: KeyName::Char('m'),
                mode: KeyMod::CTRL,
            }],
            SweepAction::Select,
        );
        key_map.register(
            &[Key {
                name: KeyName::Char('j'),
                mode: KeyMod::CTRL,
            }],
            SweepAction::Select,
        );

        // widgets
        let input = Input::new();
        let list = List::new(RankerResultThemed::new(
            theme.clone(),
            Arc::new(RankerResult::<H>::default()),
        ));

        Self {
            prompt,
            key_map_state: Vec::new(),
            key_empty_backspace: None,
            key_map,
            key_actions,
            label_face,
            separator_face,
            stats_face,
            theme,
            input,
            list,
            ranker,
        }
    }

    fn render(&mut self, mut view: impl SurfaceMut<Item = Cell>) -> Result<(), Error> {
        self.ranker.niddle_set(self.input.get().collect());
        let ranker_result = self.ranker.result();

        // label
        let mut label_view = view.view_mut(0, ..);
        let mut label = label_view.writer().face(self.label_face);
        write!(&mut label, " {} ", self.prompt)?;
        let mut label = label.face(self.separator_face);
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
        let mut stats = stats_view.writer().face(self.separator_face);
        write!(&mut stats, "")?;
        let mut stats = stats.face(self.stats_face);
        stats.write_all(stats_str.as_ref())?;

        // input
        self.input
            .render(&self.theme, view.view_mut(0, input_start..input_stop))?;

        // list
        if self.list.items().generation() != ranker_result.generation {
            let old_result = self
                .list
                .items_set(RankerResultThemed::new(self.theme.clone(), ranker_result));
            // dropping old result might add noticeable delay for large lists
            rayon::spawn(move || std::mem::drop(old_result));
        }
        self.list.render(&self.theme, view.view_mut(1.., ..))?;

        Ok(())
    }

    fn apply(&mut self, action: SweepAction) -> SweepKeyEvent<H> {
        use SweepKeyEvent::*;
        match action {
            SweepAction::Input(action) => self.input.apply(action),
            SweepAction::List(action) => self.list.apply(action),
            SweepAction::User(tag) => {
                if !tag.is_empty() {
                    return Event(SweepEvent::Bind(tag));
                }
            }
            SweepAction::Quit => {
                return SweepKeyEvent::Quit;
            }
            SweepAction::Select => match self.list.current() {
                Some(result) => {
                    return Event(SweepEvent::Select(Some(result.result.haystack)));
                }
                None => {
                    return Event(SweepEvent::Select(None));
                }
            },
            SweepAction::Help => return Help,
        }
        Nothing
    }

    fn handle_key(&mut self, key: Key) -> SweepKeyEvent<H> {
        use SweepKeyEvent::*;
        if let Some(action) = self
            .key_map
            .lookup_state(&mut self.key_map_state, key)
            .cloned()
        {
            // do not generate Backspace, when input is not empty
            let backspace = Key::new(KeyName::Backspace, KeyMod::EMPTY);
            if key == backspace && self.input.get().count() == 0 {
                if let Some(ref tag) = self.key_empty_backspace {
                    return Event(SweepEvent::Bind(tag.clone()));
                }
            } else {
                return self.apply(action);
            }
        } else if let Key {
            name: KeyName::Char(c),
            mode: KeyMod::EMPTY,
        } = key
        {
            // send plain chars to the input
            self.input.apply(InputAction::Insert(c));
        }
        Nothing
    }

    fn help_state(&self, term_waker: TerminalWaker) -> SweepState<Candidate> {
        let mut bindings = BTreeMap::new();
        for (name, action) in self.key_actions.iter() {
            bindings.insert(action.clone(), (name.to_string(), String::new()));
        }
        self.key_map.for_each(|chord, action| {
            let (_, keys) = bindings.entry(action.clone()).or_insert_with(|| {
                let name = match action {
                    SweepAction::User(tag) if !tag.is_empty() => tag.clone(),
                    _ => String::new(),
                };
                (name, String::new())
            });
            let fail = "in memory write failed";
            write!(keys, "\"").expect(fail);
            for (index, key) in chord.iter().enumerate() {
                if index != 0 {
                    write!(keys, " ").expect(fail);
                }
                write!(keys, "{}", key).expect(fail);
            }
            write!(keys, "\" ").expect(fail);
        });
        let name_len = bindings
            .values()
            .map(|(name, _)| name.len())
            .max()
            .unwrap_or(0);
        let candidates = bindings
            .into_iter()
            .map(|(_action, (name, chrod))| {
                Candidate::from_fields(
                    vec![
                        Ok(format!("{0:<1$}", name, name_len)),
                        Err(" │ ".to_owned()),
                        Ok(chrod),
                    ],
                    Some(Value::String(name)),
                )
            })
            .collect();
        let ranker = Ranker::new(
            Arc::new(|niddle: &str| {
                let niddle: Vec<_> = niddle.chars().flat_map(char::to_lowercase).collect();
                Arc::new(FuzzyScorer::new(niddle))
            }),
            false,
            move || term_waker.wake().is_ok(),
        );
        ranker.haystack_extend(candidates);
        SweepState::new("BINDINGS".to_owned(), ranker, self.theme.clone())
    }
}

fn sweep_worker<H>(
    options: SweepOptions,
    mut term: SystemTerminal,
    ranker: Ranker<H>,
    commands: Receiver<SweepCommand>,
    response: Sender<SweepResponse<H>>,
    events: Sender<SweepEvent<H>>,
) -> Result<(), Error>
where
    H: Haystack,
{
    // initialize terminal
    term.execute(TerminalCommand::DecModeSet {
        enable: false,
        mode: DecMode::VisibleCursor,
    })?;
    term.execute(TerminalCommand::Title(options.title.clone()))?;
    if options.altscreen {
        term.execute(TerminalCommand::DecModeSet {
            enable: true,
            mode: DecMode::AltScreen,
        })?;
    }
    if options.debug {
        term.duplicate_output("/tmp/sweep.log")?;
    }

    // find current row offset
    let mut row_offset = 0;
    let height = options.height;
    term.execute(TerminalCommand::CursorGet)?;
    while let Some(event) = term.poll(None)? {
        if let TerminalEvent::CursorPosition { row, .. } = event {
            row_offset = row;
            break;
        }
    }
    let term_size = term.size()?;
    if height > term_size.cells.height {
        row_offset = 0;
    } else if row_offset + height > term_size.cells.height {
        let scroll = row_offset + height - term_size.cells.height;
        row_offset = term_size.cells.height - height;
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }

    let mut state = SweepState::new(options.prompt.clone(), ranker, options.theme.clone());
    let mut state_help: Option<SweepState<Candidate>> = None;

    // render loop
    term.waker().wake()?; // schedule one wake just in case if it was consumed by previous poll
    let result = term.run_render(|term, event, view| -> Result<TerminalAction<()>, Error> {
        // handle events
        match event {
            Some(TerminalEvent::Resize(_term_size)) => {
                term.execute(TerminalCommand::Scroll(row_offset as i32))?;
                row_offset = 0;
            }
            Some(TerminalEvent::Wake) => {
                for command in commands.try_iter() {
                    match command {
                        SweepCommand::NiddleSet(niddle) => state.input.set(niddle.as_ref()),
                        SweepCommand::NiddleGet => {
                            response.send(SweepResponse::Niddle(state.input.get().collect()))?;
                        }
                        SweepCommand::Terminate => return Ok(TerminalAction::Quit(())),
                        SweepCommand::Bind(chord, tag) => match *chord.as_slice() {
                            [Key {
                                name: KeyName::Backspace,
                                mode: KeyMod::EMPTY,
                            }] => {
                                state.key_empty_backspace.replace(tag);
                            }
                            _ => {
                                let action = match state.key_actions.get(tag.as_str()) {
                                    Some(action) => action.clone(),
                                    None => SweepAction::User(tag),
                                };
                                state.key_map.register(chord.as_ref(), action);
                            }
                        },
                        SweepCommand::PromptSet(new_prompt) => {
                            state.prompt = new_prompt;
                        }
                        SweepCommand::Current => {
                            let current = state
                                .list
                                .current()
                                .map(|candidate| candidate.result.haystack);
                            response.send(SweepResponse::Current(current))?;
                        }
                    }
                }
            }
            Some(TerminalEvent::Key(key)) => {
                let action = match state_help.as_mut() {
                    None => state.handle_key(key),
                    Some(help) => match help.handle_key(key) {
                        SweepKeyEvent::Quit => {
                            state_help.take();
                            SweepKeyEvent::Nothing
                        }
                        SweepKeyEvent::Event(SweepEvent::Select(Some(bind))) => {
                            let name = bind
                                .to_json()
                                .as_str()
                                .map_or_else(String::new, ToOwned::to_owned);
                            let action = state
                                .key_actions
                                .get(name.as_str())
                                .cloned()
                                .unwrap_or(SweepAction::User(name));
                            state_help.take();
                            state.apply(action)
                        }
                        _ => SweepKeyEvent::Nothing,
                    },
                };
                match action {
                    SweepKeyEvent::Event(event) => events.send(event)?,
                    SweepKeyEvent::Quit => return Ok(TerminalAction::Quit(())),
                    SweepKeyEvent::Nothing => {}
                    SweepKeyEvent::Help => {
                        if state_help.is_none() {
                            state_help.replace(state.help_state(term.waker()));
                        }
                    }
                }
            }
            _ => (),
        }

        // render
        let view = view.view_owned((row_offset as i32)..(row_offset + height) as i32, 1..-1);
        match state_help.as_mut() {
            Some(state) => state.render(view)?,
            None => state.render(view)?,
        }

        Ok(TerminalAction::Wait)
    });

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: row_offset,
        col: 0,
    }))?;
    if options.altscreen {
        term.execute(TerminalCommand::DecModeSet {
            enable: false,
            mode: DecMode::AltScreen,
        })?;
    }
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    let _ = result?;

    Ok(())
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
