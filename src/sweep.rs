use crate::{
    fuzzy_scorer,
    rpc::{RpcError, RpcParams, RpcPeer},
    substr_scorer, Candidate, Field, Haystack, Ranker, RankerResult, ScoreResult, ScorerBuilder,
};
use anyhow::{Context, Error};
use crossbeam_channel::{unbounded, Receiver, Sender};
use futures::{
    channel::oneshot,
    future::{self, BoxFuture},
    FutureExt,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt::Write as _,
    io::Write,
    mem,
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
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};

pub const SCORER_NEXT_TAG: &str = "sweep.scorer.next";

pub struct SweepOptions {
    pub altscreen: bool,
    pub debug: bool,
    pub height: usize,
    pub keep_order: bool,
    pub prompt: String,
    pub scorers: VecDeque<ScorerBuilder>,
    pub theme: Theme,
    pub title: String,
    pub tty_path: String,
}

impl Default for SweepOptions {
    fn default() -> Self {
        let mut scorers = VecDeque::new();
        scorers.push_back(fuzzy_scorer());
        scorers.push_back(substr_scorer());
        Self {
            height: 11,
            prompt: "INPUT".to_string(),
            theme: Theme::light(),
            keep_order: false,
            tty_path: "/dev/tty".to_string(),
            title: "sweep".to_string(),
            scorers,
            altscreen: false,
            debug: false,
        }
    }
}

/// Simple sweep function when you just need to select single entry from the list
pub fn sweep<H, HS>(haystack: HS, options: Option<SweepOptions>) -> Result<Option<H>, Error>
where
    HS: IntoIterator,
    H: Haystack + From<HS::Item>,
{
    let sweep = Sweep::new(options.unwrap_or_default())?;
    sweep.items_extend(haystack);
    for event in sweep.events().iter() {
        if let SweepEvent::Select(Some(entry)) = event {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

#[derive(Debug)]
enum SweepRequest<H> {
    NiddleSet(String),
    NiddleGet(oneshot::Sender<String>),
    PromptSet(String),
    Bind(Vec<Key>, String),
    Terminate,
    Current(oneshot::Sender<Option<H>>),
    PeerSet(mpsc::UnboundedSender<SweepEvent<H>>),
    ScorerByName(Option<String>, oneshot::Sender<bool>),
}

#[derive(Clone, Debug)]
pub enum SweepEvent<H> {
    Select(Option<H>),
    Bind(String),
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

    fn send_request(&self, request: SweepRequest<H>) {
        self.requests
            .send(request)
            .expect("failed to send request to sweep_worker");
        self.waker.wake().expect("failed to wake terminal");
    }

    /// Extend list of searchable items
    pub fn items_extend<HS>(&self, items: HS)
    where
        HS: IntoIterator,
        H: From<HS::Item>,
    {
        self.ranker
            .haystack_extend(items.into_iter().map(From::from).collect())
    }

    /// Clear list of searchable items
    pub fn items_clear(&self) {
        self.ranker.haystack_clear()
    }

    /// Get currently selected candidates
    pub async fn items_current(&self) -> Result<Option<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::Current(send));
        Ok(recv.await?)
    }

    /// Reverse haystack
    pub fn items_reverse(&self) {
        self.ranker.haystack_reverse()
    }

    /// Set niddle to the spcified string
    pub fn query_set(&self, niddle: impl AsRef<str>) {
        self.send_request(SweepRequest::NiddleSet(niddle.as_ref().to_string()))
    }

    /// Get current niddle value
    pub async fn query_get(&self) -> Result<String, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::NiddleGet(send));
        Ok(recv.await?)
    }

    /// Set scorer used for ranking
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.ranker.scorer_set(scorer)
    }

    /// Swith to next scorer
    pub async fn scorer_by_name(&self, name: Option<String>) -> Result<(), Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::ScorerByName(name.clone(), send));
        if !recv.await? {
            return Err(anyhow::anyhow!("unkown scorer type: {:?}", name));
        }
        Ok(())
    }

    /// Set prompt
    pub fn prompt_set(&self, prompt: String) {
        self.send_request(SweepRequest::PromptSet(prompt))
    }

    /// Bind specified chord to the tag
    ///
    /// Whenever sequence of keys specified by chord is pressed, `SweepEvent::Bind(tag)`
    /// will be generated, note if tag is empty string the binding will be removed
    /// and no event will be generated. Tag can also be one of the standard actions
    /// list of which is available with `ctrl+h`
    pub fn bind(&self, chord: Vec<Key>, tag: String) {
        self.send_request(SweepRequest::Bind(chord, tag))
    }

    /// Event generated by the `Sweep` object
    pub fn events(&self) -> &Receiver<SweepEvent<H>> {
        &self.events
    }
}

pub struct SweepInner<H: Haystack> {
    ranker: Ranker<H>,
    waker: TerminalWaker,
    ui_worker: Option<JoinHandle<Result<(), Error>>>,
    requests: Sender<SweepRequest<H>>,
    events: Receiver<SweepEvent<H>>,
}

impl<H: Haystack> SweepInner<H> {
    pub fn new(mut options: SweepOptions) -> Result<Self, Error> {
        if options.scorers.is_empty() {
            options.scorers.push_back(fuzzy_scorer());
            options.scorers.push_back(substr_scorer());
        }
        let (requests_send, requests_recv) = unbounded();
        let (events_send, events_recv) = unbounded();
        let term = SystemTerminal::open(&options.tty_path)
            .with_context(|| format!("failed to open terminal: {}", options.tty_path))?;
        let waker = term.waker();
        let ranker = Ranker::new(options.scorers[0].clone(), options.keep_order, {
            let waker = waker.clone();
            move || waker.wake().is_ok()
        });
        let worker = Builder::new().name("sweep-ui".to_string()).spawn({
            let ranker = ranker.clone();
            move || sweep_ui_worker(options, term, ranker, requests_recv, events_send)
        })?;
        Ok(SweepInner {
            ranker,
            waker,
            ui_worker: Some(worker),
            requests: requests_send,
            events: events_recv,
        })
    }
}

impl<H> Drop for SweepInner<H>
where
    H: Haystack,
{
    fn drop(&mut self) {
        self.requests.send(SweepRequest::Terminate).unwrap_or(());
        self.waker.wake().unwrap_or(());
        if let Some(handle) = self.ui_worker.take() {
            if let Err(error) = handle.join() {
                eprintln!("sweep ui worker thread failed:\r\n{:?}", error);
            }
        }
    }
}

impl Sweep<Candidate> {
    pub fn serve<R, W>(&self, read: R, write: W) -> BoxFuture<'static, Result<(), RpcError>>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let peer = RpcPeer::new();

        // items extend
        peer.regesiter("items_extend", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let items: Vec<Candidate> = params.take(0, "items")?;
                    sweep.items_extend(items);
                    Ok(Value::Null)
                }
            }
        });

        // items clear
        peer.regesiter("items_clear", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.items_clear();
                future::ok(Value::Null)
            }
        });

        // items current
        peer.regesiter("items_current", {
            let sweep = self.clone();
            move |_params: Value| {
                let sweep = sweep.clone();
                async move {
                    let current = sweep
                        .items_current()
                        .await?
                        .and_then(|current| serde_json::to_value(current).ok())
                        .unwrap_or(Value::Null);
                    Ok(current)
                }
            }
        });

        // query set
        peer.regesiter("query_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let query: String = params.take(0, "query")?;
                    sweep.query_set(query);
                    Ok(Value::Null)
                }
            }
        });

        // query get
        peer.regesiter("query_get", {
            let sweep = self.clone();
            move |_params: Value| {
                let sweep = sweep.clone();
                async move { Ok(sweep.query_get().await?) }
            }
        });

        // terminate
        peer.regesiter("terminate", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.send_request(SweepRequest::Terminate);
                future::ok(Value::Null)
            }
        });

        // prompt set
        peer.regesiter("prompt_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let prompt: String = params.take(0, "prompt")?;
                    sweep.prompt_set(prompt);
                    Ok(Value::Null)
                }
            }
        });

        // key binding
        peer.regesiter("bind", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let key: String = params.take(0, "key")?;
                    let tag: String = params.take(1, "tag")?;
                    let chord = Key::chord(key).map_err(Error::from)?;
                    sweep.bind(chord, tag);
                    Ok(Value::Null)
                }
            }
        });

        // set as current peer
        let (send, recv) = mpsc::unbounded_channel();
        self.send_request(SweepRequest::PeerSet(send));

        // handle events and serve
        let sweep = self.clone();
        async move {
            let serve = peer.serve(read, write);
            let events = async move {
                tokio::pin!(recv);
                while let Some(event) = recv.recv().await {
                    match event {
                        SweepEvent::Bind(tag) if tag == SCORER_NEXT_TAG => {
                            sweep.scorer_by_name(None).await?;
                        }
                        SweepEvent::Bind(tag) => peer.notify_with_value("bind", json!({"tag": tag}))?,
                        SweepEvent::Select(Some(item)) => peer.notify("select", json!({"item": item}))?,
                        SweepEvent::Select(None) => {}
                    }
                }
                Ok(())
            };
            tokio::select! {
                result = serve => result,
                result = events => result,
            }
        }
        .boxed()
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

    /// Crate sweep states which renders help view
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
                let mut extra = HashMap::with_capacity(1);
                extra.insert("name".to_owned(), name.clone().into());
                Candidate::new(
                    vec![
                        Field {
                            text: format!("{0:<1$}", name, name_len).into(),
                            active: true,
                        },
                        Field {
                            text: " │ ".to_owned().into(),
                            active: false,
                        },
                        Field {
                            text: chrod.into(),
                            active: true,
                        },
                    ],
                    Some(extra),
                )
            })
            .collect();
        let ranker = Ranker::new(fuzzy_scorer(), false, move || term_waker.wake().is_ok());
        ranker.haystack_extend(candidates);
        SweepState::new("BINDINGS".to_owned(), ranker, self.theme.clone())
    }
}

fn sweep_ui_worker<H>(
    mut options: SweepOptions,
    mut term: SystemTerminal,
    ranker: Ranker<H>,
    requests: Receiver<SweepRequest<H>>,
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
    let mut state_peer: Option<mpsc::UnboundedSender<SweepEvent<H>>> = None;
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
                for request in requests.try_iter() {
                    use SweepRequest::*;
                    match request {
                        PeerSet(peer) => {
                            state_peer.replace(peer);
                        }
                        NiddleSet(niddle) => state.input.set(niddle.as_ref()),
                        NiddleGet(resolve) => {
                            mem::drop(resolve.send(state.input.get().collect()));
                        }
                        Terminate => return Ok(TerminalAction::Quit(())),
                        Bind(chord, tag) => match *chord.as_slice() {
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
                        PromptSet(new_prompt) => {
                            state.prompt = new_prompt;
                        }
                        Current(resolve) => {
                            let current = state
                                .list
                                .current()
                                .map(|candidate| candidate.result.haystack);
                            mem::drop(resolve.send(current));
                        }
                        ScorerByName(None, resolve) => {
                            options.scorers.rotate_left(1);
                            state.ranker.scorer_set(options.scorers[0].clone());
                            let _ = resolve.send(true);
                        }
                        ScorerByName(Some(name), resolve) => {
                            // find index of the scorer by its name
                            let index = options.scorers.iter().enumerate().find_map(|(i, s)| {
                                if s("").name() == name {
                                    Some(i)
                                } else {
                                    None
                                }
                            });
                            let success = match index {
                                None => false,
                                Some(index) => {
                                    options.scorers.swap(0, index);
                                    state.ranker.scorer_set(options.scorers[0].clone());
                                    true
                                }
                            };
                            let _ = resolve.send(success);
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
                                .extra()
                                .get("name")
                                .unwrap_or(&Value::Null)
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
                    SweepKeyEvent::Event(event) => {
                        if let Some(peer) = &state_peer {
                            if peer.send(event.clone()).is_err() {
                                state_peer.take();
                            }
                        }
                        events.send(event)?;
                    }
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
            if !field.active {
                writer.face_set(self.face_inactive);
                writer.write_all(field.text.as_bytes())?;
                writer.face_set(self.face_default);
            } else {
                for c in field.text.chars() {
                    if self.result.positions.contains(&index) {
                        writer.put_char(c, self.face_highlight);
                    } else {
                        writer.put_char(c, self.face_default);
                    }
                    index += 1;
                }
            }
        }
        Ok(())
    }

    fn height_hint(&self, width: usize) -> Option<usize> {
        let mut length = 0;
        for field in self.result.haystack.fields() {
            for c in field.text.chars() {
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
