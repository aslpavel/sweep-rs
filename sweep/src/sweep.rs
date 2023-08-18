use crate::{
    common::VecDeserializeSeed,
    fuzzy_scorer,
    rpc::{RpcError, RpcParams, RpcPeer},
    substr_scorer,
    widgets::{ActionDesc, Input, InputAction, List, ListAction, ListItems, Theme},
    Haystack, HaystackPreview, Ranker, RankerResult, ScoreResult, ScorerBuilder,
};
use anyhow::{Context, Error};
use crossbeam_channel::{unbounded, Receiver, Sender};
use futures::{channel::oneshot, future, stream::TryStreamExt, Stream};
use serde::{
    de::{DeserializeOwned, DeserializeSeed},
    Serialize,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    marker::PhantomData,
    mem,
    ops::Deref,
    sync::Arc,
    thread::{Builder, JoinHandle},
    time::Duration,
};
use surf_n_term::{
    encoder::ColorDepth,
    view::{Align, Container, Flex, IntoView, Margins, Text, View, ViewContext},
    Glyph, Key, KeyMap, KeyMod, KeyName, Position, Surface, SurfaceMut, SystemTerminal, Terminal,
    TerminalAction, TerminalCommand, TerminalEvent, TerminalSurfaceExt, TerminalWaker,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, Mutex},
};

lazy_static::lazy_static! {
    static ref ICONS: HashMap<String, Glyph> =
        serde_json::from_str(include_str!("./icons.json"))
            .expect("invalid icons.json file");
    static ref PROMPT_DEFAULT_ICON: &'static Glyph = ICONS.get("broom")
        .expect("failed to get prompt default icon");
    static ref KEYBOARD_ICON: &'static Glyph = ICONS.get("keyboard")
        .expect("failed to get keyboard icon");
}

pub struct SweepOptions {
    pub altscreen: bool,
    pub height: usize,
    pub keep_order: bool,
    pub prompt: String,
    pub prompt_icon: Option<Glyph>,
    pub scorers: VecDeque<ScorerBuilder>,
    pub theme: Theme,
    pub title: String,
    pub tty_path: String,
    pub border: usize,
}

impl Default for SweepOptions {
    fn default() -> Self {
        let mut scorers = VecDeque::new();
        scorers.push_back(fuzzy_scorer());
        scorers.push_back(substr_scorer());
        Self {
            height: 11,
            prompt: "INPUT".to_string(),
            prompt_icon: Some(PROMPT_DEFAULT_ICON.clone()),
            theme: Theme::light(),
            keep_order: false,
            tty_path: "/dev/tty".to_string(),
            title: "sweep".to_string(),
            scorers,
            altscreen: false,
            border: 1,
        }
    }
}

/// Simple sweep function when you just need to select single entry from the stream of items
pub async fn sweep<IS, I, E>(
    items: IS,
    items_context: I::Context,
    options: SweepOptions,
) -> Result<Option<I>, Error>
where
    IS: Stream<Item = Result<I, E>>,
    I: Haystack,
    Error: From<E>,
{
    let sweep: Sweep<I> = Sweep::new(items_context, options)?;
    let collect = sweep.items_extend_stream(items.map_err(Error::from));
    let mut collected = false; // whether all items are send sweep instance
    tokio::pin!(collect);
    loop {
        tokio::select! {
            event = sweep.next_event() => match event {
                Some(SweepEvent::Select(entry)) => return Ok(entry),
                None => return Ok(None),
                _ => continue,
            },
            collect_result = &mut collect, if !collected => {
                collected = true;
                collect_result?;
            }
        }
    }
}

#[derive(Debug)]
enum SweepRequest<H> {
    NeedleSet(String),
    NeedleGet(oneshot::Sender<String>),
    PromptSet(Option<String>, Option<Glyph>),
    Bind {
        chord: Vec<Key>,
        tag: String,
        desc: String,
    },
    Terminate,
    Current(oneshot::Sender<Option<H>>),
    PeerSet(mpsc::UnboundedSender<SweepEvent<H>>),
    ScorerByName(Option<String>, oneshot::Sender<bool>),
    PreviewSet(Option<bool>),
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
    pub fn new(haystack_context: H::Context, options: SweepOptions) -> Result<Self, Error> {
        let inner = Arc::new(SweepInner::new(options, haystack_context)?);
        Ok(Sweep { inner })
    }

    fn send_request(&self, request: SweepRequest<H>) {
        self.requests
            .send(request)
            .expect("failed to send request to sweep_worker");
        self.waker.wake().expect("failed to wake terminal");
    }

    /// Toggle preview associated with the current item
    pub fn preview_set(self, value: Option<bool>) {
        self.send_request(SweepRequest::PreviewSet(value));
    }

    /// Extend list of searchable items from iterator
    pub fn items_extend<HS>(&self, items: HS)
    where
        HS: IntoIterator,
        H: From<HS::Item>,
    {
        self.ranker
            .haystack_extend(items.into_iter().map(From::from).collect())
    }

    /// Extend list of searchable items from stream
    pub async fn items_extend_stream(
        &self,
        items: impl Stream<Item = Result<H, Error>>,
    ) -> Result<(), Error> {
        items
            .try_chunks(1024)
            .map_err(|e| e.1)
            .try_for_each(|chunk| async move {
                self.items_extend(chunk);
                Ok(())
            })
            .await
    }

    /// Clear list of searchable items
    pub fn items_clear(&self) {
        self.ranker.haystack_clear()
    }

    /// Get currently selected candidate
    pub async fn items_current(&self) -> Result<Option<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::Current(send));
        Ok(recv.await?)
    }

    /// Reverse haystack
    pub fn items_reverse(&self) {
        self.ranker.haystack_reverse()
    }

    /// Set needle to the specified string
    pub fn query_set(&self, needle: impl AsRef<str>) {
        self.send_request(SweepRequest::NeedleSet(needle.as_ref().to_string()))
    }

    /// Get current needle value
    pub async fn query_get(&self) -> Result<String, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::NeedleGet(send));
        Ok(recv.await?)
    }

    /// Set scorer used for ranking
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.ranker.scorer_set(scorer)
    }

    /// Switch scorer, if name is not provided next scorer is chosen
    pub async fn scorer_by_name(&self, name: Option<String>) -> Result<(), Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::ScorerByName(name.clone(), send));
        if !recv.await? {
            return Err(anyhow::anyhow!("unkown scorer type: {:?}", name));
        }
        Ok(())
    }

    /// Set prompt
    pub fn prompt_set(&self, prompt: Option<String>, icon: Option<Glyph>) {
        self.send_request(SweepRequest::PromptSet(prompt, icon))
    }

    /// Bind specified chord to the tag
    ///
    /// Whenever sequence of keys specified by chord is pressed, [SweepEvent::Bind]
    /// will be generated, note if tag is empty string the binding will be removed
    /// and no event will be generated. Tag can also be a standard action name
    /// (see available with `ctrl+h`) in this case [SweepEvent::Bind] is not generated.
    pub fn bind(&self, chord: Vec<Key>, tag: String, desc: String) {
        self.send_request(SweepRequest::Bind { chord, tag, desc })
    }

    /// Wait for single event in the asynchronous context
    pub async fn next_event(&self) -> Option<SweepEvent<H>> {
        let mut receiver = self.events.lock().await;
        receiver.recv().await
    }
}

pub struct SweepInner<H: Haystack> {
    ranker: Ranker<H>,
    waker: TerminalWaker,
    ui_worker: Option<JoinHandle<Result<(), Error>>>,
    requests: Sender<SweepRequest<H>>,
    events: Mutex<mpsc::UnboundedReceiver<SweepEvent<H>>>,
}

impl<H: Haystack> SweepInner<H> {
    pub fn new(mut options: SweepOptions, haystack_context: H::Context) -> Result<Self, Error> {
        if options.scorers.is_empty() {
            options.scorers.push_back(fuzzy_scorer());
            options.scorers.push_back(substr_scorer());
        }
        let (requests_send, requests_recv) = unbounded();
        let (events_send, events_recv) = mpsc::unbounded_channel();
        let term = SystemTerminal::open(&options.tty_path)
            .with_context(|| format!("failed to open terminal: {}", options.tty_path))?;
        let waker = term.waker();
        let ranker = Ranker::new({
            let waker = waker.clone();
            move |_| waker.wake().is_ok()
        });
        ranker.scorer_set(options.scorers[0].clone());
        ranker.keep_order(Some(options.keep_order));
        let worker = Builder::new().name("sweep-ui".to_string()).spawn({
            let ranker = ranker.clone();
            move || {
                sweep_ui_worker(
                    options,
                    term,
                    ranker,
                    requests_recv,
                    events_send,
                    haystack_context,
                )
            }
        })?;
        Ok(SweepInner {
            ranker,
            waker,
            ui_worker: Some(worker),
            requests: requests_send,
            events: Mutex::new(events_recv),
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
                tracing::error!("[SweepInner.drop] ui worker thread failed: {:?}", error);
            }
        }
    }
}

impl<H> Sweep<H>
where
    H: Haystack + Serialize + DeserializeOwned,
{
    /// Serve RPC endpoint via read/write
    pub fn serve<'a, R, W, F>(
        &self,
        read: R,
        write: W,
        setup: F,
    ) -> impl Future<Output = Result<(), RpcError>> + 'a
    where
        R: AsyncRead + 'a,
        W: AsyncWrite + 'a,
        F: FnOnce(RpcPeer),
    {
        self.serve_seed(PhantomData::<H>, read, write, setup)
    }
}

impl<H> Sweep<H>
where
    H: Haystack + Serialize,
{
    /// Serve RPC endpoint via read/write with haystack deserialization seed
    pub fn serve_seed<'de, 'a, S, R, W, F>(
        &self,
        seed: S,
        read: R,
        write: W,
        setup: F,
    ) -> impl Future<Output = Result<(), RpcError>> + 'a
    where
        S: DeserializeSeed<'de, Value = H> + Clone + Send + Sync + 'static,
        R: AsyncRead + 'a,
        W: AsyncWrite + 'a,
        F: FnOnce(RpcPeer),
    {
        let peer = RpcPeer::new();

        // items extend
        peer.register("items_extend", {
            let sweep = self.clone();
            let seed = seed.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                let seed = seed.clone();
                async move {
                    let items = params.take_seed(VecDeserializeSeed(seed), 0, "items")?;
                    sweep.items_extend(items);
                    Ok(Value::Null)
                }
            }
        });

        // items clear
        peer.register("items_clear", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.items_clear();
                future::ok(Value::Null)
            }
        });

        // items current
        peer.register("items_current", {
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
        peer.register("query_set", {
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
        peer.register("query_get", {
            let sweep = self.clone();
            move |_params: Value| {
                let sweep = sweep.clone();
                async move { Ok(sweep.query_get().await?) }
            }
        });

        // terminate
        peer.register("terminate", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.send_request(SweepRequest::Terminate);
                future::ok(Value::Null)
            }
        });

        // prompt set
        peer.register("prompt_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let prompt: Option<String> = params.take_opt(0, "prompt")?;
                    let icon: Option<Glyph> = params.take_opt(1, "icon")?;
                    sweep.prompt_set(prompt, icon);
                    Ok(Value::Null)
                }
            }
        });

        // key binding
        peer.register("bind", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let key: String = params.take(0, "key")?;
                    let tag: String = params.take(1, "tag")?;
                    let desc: Option<String> = params.take_opt(3, "desc")?;
                    let chord = Key::chord(key).map_err(Error::from)?;
                    sweep.bind(chord, tag, desc.unwrap_or_default());
                    Ok(Value::Null)
                }
            }
        });

        // preview set
        peer.register("preview_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let value: Option<bool> = params.take_opt(0, "value")?;
                    sweep.preview_set(value);
                    Ok(Value::Null)
                }
            }
        });

        // setup
        setup(peer.clone());

        // set as current peer
        let (send, recv) = mpsc::unbounded_channel();
        self.send_request(SweepRequest::PeerSet(send));

        // handle events and serve
        async move {
            let serve = peer.serve(read, write);
            let events = async move {
                // ready event
                peer.notify_with_value(
                    "ready",
                    json!({
                        "version": [
                            env!("CARGO_PKG_VERSION_MAJOR"),
                            env!("CARGO_PKG_VERSION_MINOR"),
                            env!("CARGO_PKG_VERSION_PATCH"),
                        ]
                    }),
                )?;

                tokio::pin!(recv);
                while let Some(event) = recv.recv().await {
                    match event {
                        SweepEvent::Bind(tag) => {
                            peer.notify_with_value("bind", json!({ "tag": tag }))?
                        }
                        SweepEvent::Select(Some(item)) => {
                            peer.notify("select", json!({ "item": item }))?
                        }
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
    }
}

/// User bindable actions
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SweepAction {
    User {
        chord: Vec<Key>,
        tag: String,
        desc: String,
    },
    Select,
    Quit,
    Help,
    ScorerNext,
    PreviewToggle,
    Input(InputAction),
    List(ListAction),
}

impl SweepAction {
    fn description(&self) -> ActionDesc {
        use SweepAction::*;
        match self {
            User { chord, tag, desc } => ActionDesc {
                chords: vec![chord.clone()],
                name: tag.clone(),
                description: desc.clone(),
            },
            Select => ActionDesc {
                chords: vec![
                    vec![Key {
                        name: KeyName::Char('m'),
                        mode: KeyMod::CTRL,
                    }],
                    vec![Key {
                        name: KeyName::Char('j'),
                        mode: KeyMod::CTRL,
                    }],
                    vec![Key {
                        name: KeyName::Enter,
                        mode: KeyMod::EMPTY,
                    }],
                ],
                name: "sweep.select".to_owned(),
                description: "Select item pointed by cursor".to_owned(),
            },
            Quit => ActionDesc {
                chords: vec![
                    vec![Key {
                        name: KeyName::Char('c'),
                        mode: KeyMod::CTRL,
                    }],
                    vec![Key {
                        name: KeyName::Esc,
                        mode: KeyMod::EMPTY,
                    }],
                ],
                name: "sweep.quit".to_string(),
                description: "Close sweep".to_string(),
            },
            Help => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('h'),
                    mode: KeyMod::CTRL,
                }]],
                name: "sweep.help".to_owned(),
                description: "Show help menu".to_owned(),
            },
            ScorerNext => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('s'),
                    mode: KeyMod::CTRL,
                }]],
                name: "sweep.scorer.next".to_owned(),
                description: "Switch to next available scorer".to_owned(),
            },
            PreviewToggle => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('p'),
                    mode: KeyMod::ALT,
                }]],
                name: "sweep.preview.toggle".to_owned(),
                description: "Toggle preview for an item".to_owned(),
            },
            Input(input_action) => input_action.description(),
            List(list_action) => list_action.description(),
        }
    }

    fn all() -> impl Iterator<Item = SweepAction> {
        use SweepAction::*;
        [Select, Quit, Help, ScorerNext, PreviewToggle]
            .into_iter()
            .chain(InputAction::all().map(Input))
            .chain(ListAction::all().map(List))
    }
}

/// Object representing current state of the sweep worker
struct SweepState<H: Haystack> {
    // scorer builder
    scorers: VecDeque<ScorerBuilder>,
    // sweep prompt
    prompt: String,
    // prompt icon
    prompt_icon: Option<Glyph>,
    // current state of the key chord
    key_map_state: Vec<Key>,
    // user action executed on backspace when input is empty
    key_empty_backspace: Option<String>,
    // action key map
    key_map: KeyMap<SweepAction>,
    // action name to sweep action
    key_actions: HashMap<String, SweepAction>,
    // theme
    theme: Theme,
    // input widget
    input: Input,
    // list widget
    list: List<SweepItems<H>>,
    // ranker
    ranker: Ranker<H>,
    // Filed refs (fields that can be used as a base)
    haystack_context: H::Context,
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
    fn new(
        prompt: String,
        prompt_icon: Option<Glyph>,
        ranker: Ranker<H>,
        theme: Theme,
        scorers: VecDeque<ScorerBuilder>,
        haystack_context: H::Context,
    ) -> Self {
        // key map
        let mut key_map = KeyMap::new();
        let mut key_actions = HashMap::new();
        for action in SweepAction::all() {
            let desc = action.description();
            key_actions.insert(desc.name, action.clone());
            for chord in desc.chords {
                key_map.register(chord.as_slice(), action.clone());
            }
        }

        // widgets
        let input = Input::new(theme.clone());
        let list = List::new(
            SweepItems::new(
                Arc::new(RankerResult::<H>::default()),
                haystack_context.clone(),
            ),
            theme.clone(),
        );

        Self {
            scorers,
            prompt,
            prompt_icon,
            key_map_state: Vec::new(),
            key_empty_backspace: None,
            key_map,
            key_actions,
            theme,
            input,
            list,
            ranker,
            haystack_context,
        }
    }

    // get preview of the currently pointed haystack item
    fn preview(&self) -> Option<HaystackPreview> {
        self.list.current().and_then(|item| {
            item.result.haystack.preview(
                &self.haystack_context,
                &item.result.positions,
                &self.theme,
            )
        })
    }

    fn theme_set(&mut self, theme: Theme) {
        self.input.theme_set(theme.clone());
        self.list.theme_set(theme.clone());
        self.theme = theme;
    }

    // peek scorer by name, or next
    fn scorer_by_name(&mut self, name: Option<String>) -> bool {
        match name {
            None => {
                self.scorers.rotate_left(1);
                self.ranker.scorer_set(self.scorers[0].clone());
                true
            }
            Some(name) => {
                // find index of the scorer by its name
                let index = self.scorers.iter().enumerate().find_map(|(i, s)| {
                    if s("").name() == name {
                        Some(i)
                    } else {
                        None
                    }
                });
                match index {
                    None => false,
                    Some(index) => {
                        self.scorers.swap(0, index);
                        self.ranker.scorer_set(self.scorers[0].clone());
                        true
                    }
                }
            }
        }
    }

    fn apply(&mut self, action: SweepAction) -> SweepKeyEvent<H> {
        use SweepKeyEvent::*;
        match action {
            SweepAction::Input(action) => self.input.apply(action),
            SweepAction::List(action) => self.list.apply(action),
            SweepAction::User { tag, .. } => {
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
            SweepAction::ScorerNext => {
                self.scorer_by_name(None);
                return Nothing;
            }
            SweepAction::PreviewToggle => self.theme_set(Theme {
                show_preview: !self.theme.show_preview,
                ..self.theme.clone()
            }),
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
    fn help_state(&self, term_waker: TerminalWaker) -> SweepState<ActionDesc> {
        // Tag -> ActionDesc
        let mut descriptions: BTreeMap<String, ActionDesc> = BTreeMap::new();
        self.key_map.for_each(|chord, action| {
            let mut desc = action.description();
            if desc.name.is_empty() {
                return;
            }
            descriptions
                .entry(desc.name.clone())
                .and_modify(|desc_curr| desc_curr.chords.push(chord.to_owned()))
                .or_insert_with(|| {
                    desc.chords.clear();
                    desc.chords.push(chord.to_owned());
                    desc
                });
        });
        let mut entries: Vec<_> = descriptions.into_values().collect();
        entries.sort_by_key(|desc| self.key_actions.get(&desc.name));

        let ranker = Ranker::new(move |_| term_waker.wake().is_ok());
        ranker.keep_order(Some(true));
        ranker.haystack_extend(entries);
        SweepState::new(
            "BINDINGS".to_owned(),
            Some(KEYBOARD_ICON.clone()),
            ranker,
            Theme {
                show_preview: true,
                ..self.theme.clone()
            },
            self.scorers.clone(),
            (),
        )
    }
}

impl<'a, H: Haystack> IntoView for &'a mut SweepState<H> {
    type View = Flex<'a>;

    fn into_view(self) -> Self::View {
        // stats view
        let ranker_result = self.ranker.result();
        let stats = Text::new()
            .push_text(&self.theme.separator_left)
            .set_face(self.theme.stats)
            .push_fmt(format_args!(
                " {}/{} {:.2?}",
                ranker_result.len(),
                ranker_result.haystack_len(),
                ranker_result.duration(),
            ))
            .with_face(Default::default(), |text| {
                let name = ranker_result.scorer().name();
                match ICONS.get(name) {
                    Some(glyph) => text.put_glyph(glyph.clone()),
                    None => text.push_str(name, None),
                };
            })
            .take();

        // rank new data and update item list if needed
        self.ranker.needle_set(self.input.get().collect());
        if self.list.items().generation() != ranker_result.generation() {
            // find cursor position of currently pointed item in the new result
            let cursor = if self.list.cursor() == 0 {
                None
            } else {
                self.list
                    .items()
                    .ranker_result
                    .get_haystack_index(self.list.cursor())
                    .and_then(|haystack_index| ranker_result.find_match_index(haystack_index))
            };
            // update list with new results
            let old_items = self.list.items_set(SweepItems::new(
                ranker_result,
                self.haystack_context.clone(),
            ));
            if let Some(cursor) = cursor {
                self.list.cursor_set(cursor);
            }
            // dropping old result might add noticeable delay for large lists
            rayon::spawn(move || std::mem::drop(old_items));
        }

        // prompt
        let prompt = Text::new()
            .set_face(self.theme.label)
            .with_face(Default::default(), |text| {
                match &self.prompt_icon {
                    Some(icon) => text.put_glyph(icon.clone()),
                    None => text.put_char(' '),
                };
            })
            .push_str(self.prompt.as_str(), None)
            .put_char(' ')
            .push_text(&self.theme.separator_right)
            .take();

        // header
        let header = Flex::row()
            .add_child(prompt)
            .add_flex_child(1.0, &self.input)
            .add_child(stats);

        // list
        let mut body = Flex::row();
        body.push_flex_child(1.0, &self.list);
        // preview
        if self.theme.show_preview {
            if let Some(preview) = self.preview() {
                let flex = preview.flex.unwrap_or(0.0);
                let mut view = Container::new(preview.view)
                    .with_margins(Margins {
                        left: 1,
                        right: 1,
                        ..Default::default()
                    })
                    .with_vertical(Align::Expand)
                    .with_face(self.theme.list_selected);
                if flex > 0.0 {
                    view = view.with_horizontal(Align::Expand);
                }
                body.push_flex_child(flex, view);
            }
        }
        // scroll bar
        body.push_child(self.list.scroll_bar());

        Flex::column()
            .add_child(Container::new(header).with_height(1))
            .add_flex_child(1.0, body)
    }
}

fn sweep_ui_worker<H>(
    mut options: SweepOptions,
    mut term: SystemTerminal,
    ranker: Ranker<H>,
    requests: Receiver<SweepRequest<H>>,
    events: mpsc::UnboundedSender<SweepEvent<H>>,
    haystack_context: H::Context,
) -> Result<(), Error>
where
    H: Haystack,
{
    tracing::debug!(?options.theme, "[sweep_ui_worker]");

    // initialize terminal
    term.execute_many([
        TerminalCommand::visible_cursor_set(false),
        TerminalCommand::Title(options.title.clone()),
    ])?;
    if options.altscreen {
        term.execute(TerminalCommand::altscreen_set(true))?;
    }

    // Force dumb four color theme for dumb terminal
    if ColorDepth::Gray == term.capabilities().depth {
        options.theme = Theme {
            show_preview: options.theme.show_preview,
            ..Theme::dumb()
        }
    }

    // find current row offset
    let mut row_offset = term.position()?.row;
    let term_size = term.size()?;
    let height = options.height;
    if height > term_size.cells.height {
        row_offset = 0;
    } else if row_offset + height > term_size.cells.height {
        let scroll = row_offset + height - term_size.cells.height;
        row_offset = term_size.cells.height - height;
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }

    let mut state = SweepState::new(
        options.prompt.clone(),
        options.prompt_icon.clone(),
        ranker,
        options.theme.clone(),
        options.scorers,
        haystack_context,
    );
    let mut state_peer: Option<mpsc::UnboundedSender<SweepEvent<H>>> = None;
    let mut state_help: Option<SweepState<ActionDesc>> = None;

    // render loop
    term.waker().wake()?; // schedule one wake just in case if it was consumed by previous poll
    let result = term.run_render(|term, event, mut view| {
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
                        NeedleSet(needle) => state.input.set(needle.as_ref()),
                        NeedleGet(resolve) => {
                            mem::drop(resolve.send(state.input.get().collect()));
                        }
                        Terminate => return Ok(TerminalAction::Quit(())),
                        Bind { chord, tag, desc } => match *chord.as_slice() {
                            [Key {
                                name: KeyName::Backspace,
                                mode: KeyMod::EMPTY,
                            }] => {
                                state.key_empty_backspace =
                                    if tag.is_empty() { None } else { Some(tag) };
                            }
                            _ => {
                                let action = if tag.is_empty() {
                                    // empty user action means unbind
                                    SweepAction::User {
                                        chord: Vec::new(),
                                        tag: String::new(),
                                        desc: String::new(),
                                    }
                                } else {
                                    state
                                        .key_actions
                                        .entry(tag.clone())
                                        .or_insert_with(|| SweepAction::User {
                                            chord: chord.clone(),
                                            tag,
                                            desc,
                                        })
                                        .clone()
                                };
                                state.key_map.register(chord.as_ref(), action);
                            }
                        },
                        PromptSet(new_prompt, new_icon) => {
                            if let Some(new_prompt) = new_prompt {
                                state.prompt = new_prompt;
                            }
                            state.prompt_icon = new_icon;
                        }
                        Current(resolve) => {
                            let current = state.list.current().map(|item| item.result.haystack);
                            mem::drop(resolve.send(current));
                        }
                        ScorerByName(name, resolve) => {
                            let _ = resolve.send(state.scorer_by_name(name));
                        }
                        PreviewSet(value) => {
                            let show_preview = match value {
                                Some(value) => value,
                                None => !state.theme.show_preview,
                            };
                            state.theme_set(Theme {
                                show_preview,
                                ..state.theme.clone()
                            });
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
                        SweepKeyEvent::Event(SweepEvent::Select(Some(action_desc))) => {
                            state_help.take();
                            if let Some(action) = state.key_actions.get(&action_desc.name).cloned()
                            {
                                state.apply(action)
                            } else {
                                SweepKeyEvent::Nothing
                            }
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
        let mut state_surf = if options.border > 0 && options.border < view.width() / 2 {
            let border = options.border as i32;
            view.view_mut(
                (row_offset as i32)..(row_offset + height) as i32,
                border..-border,
            )
        } else {
            view.view_mut((row_offset as i32)..(row_offset + height) as i32, ..)
        };
        let ctx = ViewContext::new(term)?;
        if let Some(state) = state_help.as_mut() {
            tracing::debug_span!("[sweep_ui_worker][draw] sweep help state")
                .in_scope(|| state_surf.draw_view(&ctx, state))?;
        } else {
            tracing::debug_span!("[sweep_ui_worker][draw] sweep state")
                .in_scope(|| state_surf.draw_view(&ctx, &mut state))?;
        }

        Ok(TerminalAction::Wait)
    });

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: row_offset,
        col: 0,
    }))?;
    if options.altscreen {
        term.execute(TerminalCommand::altscreen_set(false))?;
    }
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    result
}

struct SweepItems<H: Haystack> {
    ranker_result: Arc<RankerResult<H>>,
    haystack_context: H::Context,
}

impl<H: Haystack> SweepItems<H> {
    fn new(ranker_result: Arc<RankerResult<H>>, haystack_context: H::Context) -> Self {
        Self {
            ranker_result,
            haystack_context,
        }
    }

    fn generation(&self) -> usize {
        self.ranker_result.generation()
    }
}

impl<H: Haystack> ListItems for SweepItems<H> {
    type Item = SweepItem<H>;

    fn len(&self) -> usize {
        self.ranker_result.len()
    }

    fn get(&self, index: usize, theme: Theme) -> Option<Self::Item> {
        self.ranker_result.get(index).map(|result| SweepItem {
            result: result.clone(),
            theme,
            haystack_context: self.haystack_context.clone(),
        })
    }
}

struct SweepItem<H: Haystack> {
    result: ScoreResult<H>,
    theme: Theme,
    haystack_context: H::Context,
}

impl<H: Haystack> IntoView for SweepItem<H> {
    type View = Box<dyn View>;

    fn into_view(self) -> Self::View {
        self.result
            .haystack
            .view(&self.haystack_context, &self.result.positions, &self.theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_icons_parsing() {
        let _ = ICONS.len();
    }
}
