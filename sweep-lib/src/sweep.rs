use crate::{
    common::{LockExt, VecDeserializeSeed},
    fuzzy_scorer,
    rank::{RankedItem, RankedItemId},
    rpc::{RpcError, RpcParams, RpcPeer},
    substr_scorer,
    widgets::{ActionDesc, Input, InputAction, List, ListAction, ListItems, Theme},
    Haystack, HaystackPreview, RankedItems, Ranker, ScorerBuilder,
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
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, RwLock,
    },
    thread::{Builder, JoinHandle},
    time::Duration,
};
use surf_n_term::{
    encoder::ColorDepth,
    view::{
        Align, Axis, Container, Flex, IntoView, Layout, Margins, ScrollBarFn, ScrollBarPosition,
        Text, Tree, TreeId, TreeView, View, ViewCache, ViewContext, ViewDeserializer,
        ViewLayoutStore,
    },
    CellWrite, Face, FaceAttrs, Glyph, Key, KeyChord, KeyMap, KeyMod, KeyName, Position, Size,
    SystemTerminal, Terminal, TerminalAction, TerminalCommand, TerminalEvent, TerminalSize,
    TerminalSurfaceExt, TerminalWaker,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, Mutex},
};

lazy_static::lazy_static! {
    static ref ICONS: HashMap<String, Glyph> =
        serde_json::from_str(include_str!("./icons.json"))
            .expect("invalid icons.json file");
    pub static ref PROMPT_DEFAULT_ICON: &'static Glyph = ICONS.get("prompt")
        .expect("failed to get prompt default icon");
    static ref KEYBOARD_ICON: &'static Glyph = ICONS.get("keyboard")
        .expect("failed to get keyboard icon");
}
const SWEEP_SCORER_NEXT_TAG: &str = "sweep.scorer.next";

pub struct SweepOptions {
    pub keep_order: bool,
    pub prompt: String,
    pub prompt_icon: Option<Glyph>,
    pub scorers: VecDeque<ScorerBuilder>,
    pub theme: Theme,
    pub title: String,
    pub tty_path: String,
    pub layout: SweepLayout,
}

impl Default for SweepOptions {
    fn default() -> Self {
        let mut scorers = VecDeque::new();
        scorers.push_back(fuzzy_scorer());
        scorers.push_back(substr_scorer());
        Self {
            prompt: "INPUT".to_string(),
            prompt_icon: Some(PROMPT_DEFAULT_ICON.clone()),
            theme: Theme::light(),
            keep_order: false,
            tty_path: "/dev/tty".to_string(),
            title: "sweep".to_string(),
            scorers,
            layout: SweepLayout::default(),
        }
    }
}

/// Simple sweep function when you just need to select single entry from the stream of items
pub async fn sweep<IS, I, E>(
    items: IS,
    items_context: I::Context,
    options: SweepOptions,
) -> Result<Vec<I>, Error>
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
                None => return Ok(Vec::new()),
                _ => continue,
            },
            collect_result = &mut collect, if !collected => {
                collected = true;
                collect_result?;
            }
        }
    }
}

enum SweepRequest<H> {
    NeedleSet(String),
    NeedleGet(oneshot::Sender<String>),
    PromptSet(Option<String>, Option<Glyph>),
    ThemeGet(oneshot::Sender<Theme>),
    Bind {
        chord: KeyChord,
        tag: String,
        desc: String,
    },
    Terminate,
    Current(oneshot::Sender<Option<H>>),
    Marked(oneshot::Sender<Vec<H>>),
    CursorSet {
        position: usize,
    },
    ScorerByName(Option<String>, oneshot::Sender<bool>),
    ScorerSet(ScorerBuilder),
    PreviewSet(Option<bool>),
    FooterSet(Option<Arc<dyn View>>),
    HaystackExtend(Vec<H>),
    HaystackUpdate {
        index: usize,
        item: H,
    },
    HaystackClear,
    HaystackReverse,
    RankerKeepOrder(Option<bool>),
    StatePush,
    StatePop,
    RenderSuppress(bool),
}

#[derive(Clone, Debug)]
pub enum SweepEvent<H> {
    Select(Vec<H>),
    Bind { tag: String, chord: KeyChord },
    Resize(TerminalSize),
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

    /// Get terminal waker
    pub fn waker(&self) -> TerminalWaker {
        self.waker.clone()
    }

    /// Create new state and put on the top of the stack of active states
    pub fn state_push(&self) {
        self.send_request(SweepRequest::StatePush)
    }

    /// Remove state at the top of the stack and active one below it
    pub fn state_pop(&self) {
        self.send_request(SweepRequest::StatePop)
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
        let items = items.into_iter().map(From::from).collect();
        self.send_request(SweepRequest::HaystackExtend(items))
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

    /// Update item by its index
    pub fn item_update(&self, index: usize, item: H) {
        self.send_request(SweepRequest::HaystackUpdate { index, item })
    }

    /// Clear list of searchable items
    pub fn items_clear(&self) {
        self.send_request(SweepRequest::HaystackClear)
    }

    /// Get currently selected items
    pub async fn items_current(&self) -> Result<Option<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::Current(send));
        Ok(recv.await?)
    }

    /// Get marked (multi-select) items
    pub async fn items_marked(&self) -> Result<Vec<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::Marked(send));
        Ok(recv.await?)
    }

    /// Reverse haystack
    pub fn items_reverse(&self) {
        self.send_request(SweepRequest::HaystackReverse)
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
        self.send_request(SweepRequest::ScorerSet(scorer))
    }

    /// Whether to keep order of elements or not
    pub fn keep_order(&self, toggle: Option<bool>) {
        self.send_request(SweepRequest::RankerKeepOrder(toggle))
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

    /// Get current theme
    pub async fn theme_get(&self) -> Result<Theme, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::ThemeGet(send));
        Ok(recv.await?)
    }

    /// Set footer
    pub fn footer_set(&self, footer: Option<Arc<dyn View>>) {
        self.send_request(SweepRequest::FooterSet(footer))
    }

    /// Set cursor to specified position
    pub fn cursor_set(&self, position: usize) {
        self.send_request(SweepRequest::CursorSet { position })
    }

    /// Bind specified chord to the tag
    ///
    /// Whenever sequence of keys specified by chord is pressed, [SweepEvent::Bind]
    /// will be generated, note if tag is empty string the binding will be removed
    /// and no event will be generated. Tag can also be a standard action name
    /// (see available with `ctrl+h`) in this case [SweepEvent::Bind] is not generated.
    pub fn bind(&self, chord: KeyChord, tag: String, desc: String) {
        self.send_request(SweepRequest::Bind { chord, tag, desc })
    }

    /// Suppress rendering to reduce flickering
    pub fn render_suppress(&self, suppress: bool) {
        self.send_request(SweepRequest::RenderSuppress(suppress))
    }

    /// Wait for single event in the asynchronous context
    pub async fn next_event(&self) -> Option<SweepEvent<H>> {
        let mut receiver = self.events.lock().await;
        receiver.recv().await
    }

    /// Wait for sweep to correctly terminate and cleanup terminal
    pub async fn terminate(&self) {
        let _ = self.requests.send(SweepRequest::Terminate);
        let _ = self.waker.wake();
        if let Some(terminated) = self.terminated.with_mut(|t| t.take()) {
            let _ = terminated.await;
        }
    }
}

pub struct SweepInner<H: Haystack> {
    waker: TerminalWaker,
    ui_worker: Option<JoinHandle<Result<(), Error>>>,
    requests: Sender<SweepRequest<H>>,
    events: Mutex<mpsc::UnboundedReceiver<SweepEvent<H>>>,
    terminated: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl<H: Haystack> SweepInner<H> {
    pub fn new(mut options: SweepOptions, haystack_context: H::Context) -> Result<Self, Error> {
        if options.scorers.is_empty() {
            options.scorers.push_back(fuzzy_scorer());
            options.scorers.push_back(substr_scorer());
        }
        let (requests_send, requests_recv) = unbounded();
        let (events_send, events_recv) = mpsc::unbounded_channel();
        let (terminate_send, terminate_recv) = oneshot::channel();
        let term = SystemTerminal::open(&options.tty_path)
            .with_context(|| format!("failed to open terminal: {}", options.tty_path))?;
        let waker = term.waker();
        let worker = Builder::new().name("sweep-ui".to_string()).spawn({
            move || {
                sweep_ui_worker(options, term, requests_recv, events_send, haystack_context)
                    .inspect(|_result| {
                        let _ = terminate_send.send(());
                    })
            }
        })?;
        Ok(SweepInner {
            waker,
            ui_worker: Some(worker),
            requests: requests_send,
            events: Mutex::new(events_recv),
            terminated: std::sync::Mutex::new(Some(terminate_recv)),
        })
    }
}

impl<H> Drop for SweepInner<H>
where
    H: Haystack,
{
    fn drop(&mut self) {
        let _ = self.requests.send(SweepRequest::Terminate);
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
        view_cache: Option<Arc<dyn ViewCache>>,
        read: R,
        write: W,
        setup: F,
    ) -> impl Future<Output = Result<(), RpcError>> + 'a
    where
        R: AsyncRead + 'a,
        W: AsyncWrite + 'a,
        F: FnOnce(RpcPeer),
    {
        self.serve_seed(PhantomData::<H>, view_cache, read, write, setup)
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
        view_cache: Option<Arc<dyn ViewCache>>,
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

        // item update
        peer.register("item_update", {
            let sweep = self.clone();
            let seed = seed.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                let seed = seed.clone();
                async move {
                    let index = params.take(0, "index")?;
                    let item = params.take_seed(seed, 1, "item")?;
                    sweep.item_update(index, item);
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

        // items marked
        peer.register("items_marked", {
            let sweep = self.clone();
            move |_params: Value| {
                let sweep = sweep.clone();
                async move {
                    let items = serde_json::to_value(sweep.items_marked().await?)?;
                    Ok(items)
                }
            }
        });

        peer.register("cursor_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let position: usize = params.take(0, "position")?;
                    sweep.cursor_set(position);
                    Ok(Value::Null)
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

        // footer set
        peer.register("footer_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                let view_cache = view_cache.clone();
                async move {
                    let theme = sweep.theme_get().await?;
                    let seed = ViewDeserializer::new(Some(&theme.named_colors), view_cache);
                    let footer: Option<Arc<dyn View>> = params.take_opt_seed(&seed, 0, "footer")?;
                    sweep.footer_set(footer.map(Arc::from));
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
                    let chord: KeyChord = params.take(0, "key")?;
                    let tag: String = params.take(1, "tag")?;
                    let desc: Option<String> = params.take_opt(2, "desc")?;
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

        // state_push
        peer.register("state_push", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.state_push();
                future::ok(Value::Null)
            }
        });

        // state_push
        peer.register("state_pop", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.state_pop();
                future::ok(Value::Null)
            }
        });

        // render_suppress
        peer.register("render_suppress", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    sweep.render_suppress(params.take(0, "suppress")?);
                    Ok(Value::Null)
                }
            }
        });

        // setup
        setup(peer.clone());

        // handle events and serve
        let sweep = self.clone();
        let sweep_terminate = self.clone();
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

                while let Some(event) = sweep.next_event().await {
                    match event {
                        SweepEvent::Bind { tag, chord } => {
                            peer.notify_with_value("bind", json!({"tag": tag, "key": chord}))?
                        }
                        SweepEvent::Select(items) => {
                            if !items.is_empty() {
                                peer.notify_with_value("select", json!({"items": items}))?
                            }
                        }
                        SweepEvent::Resize(size) => peer.notify_with_value(
                            "resize",
                            json!({
                                "cells": size.cells,
                                "pixels": size.pixels,
                                "pixels_per_cell": size.pixels_per_cell(),
                            }),
                        )?,
                    }
                }
                Ok(())
            };
            let result = tokio::select! {
                result = serve => result,
                result = events => result,
            };
            sweep_terminate.terminate().await;
            result
        }
    }
}

/// User bindable actions
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum SweepAction {
    User {
        chord: KeyChord,
        tag: String,
        desc: String,
    },
    Select,
    Mark,
    MarkAll,
    Quit,
    Help,
    ScorerNext,
    PreviewToggle,
    PreviewLineNext,
    PreviewLinePrev,
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
                    KeyChord::from_iter([Key {
                        name: KeyName::Char('m'),
                        mode: KeyMod::CTRL,
                    }]),
                    KeyChord::from_iter([Key {
                        name: KeyName::Char('j'),
                        mode: KeyMod::CTRL,
                    }]),
                    KeyChord::from_iter([Key {
                        name: KeyName::Enter,
                        mode: KeyMod::EMPTY,
                    }]),
                ],
                name: "sweep.select".to_owned(),
                description: "Select item pointed by cursor".to_owned(),
            },
            Mark => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('m'),
                    mode: KeyMod::ALT,
                }])],
                name: "sweep.mark.current".to_owned(),
                description: "Mark item pointed by cursor".to_owned(),
            },
            MarkAll => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('m'),
                    mode: KeyMod::ALT | KeyMod::SHIFT,
                }])],
                name: "sweep.mark.all".to_owned(),
                description: "(Un)Mark all filtered items".to_owned(),
            },
            Quit => ActionDesc {
                chords: vec![
                    KeyChord::from_iter([Key {
                        name: KeyName::Char('c'),
                        mode: KeyMod::CTRL,
                    }]),
                    KeyChord::from_iter([Key {
                        name: KeyName::Esc,
                        mode: KeyMod::EMPTY,
                    }]),
                ],
                name: "sweep.quit".to_string(),
                description: "Close sweep".to_string(),
            },
            Help => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('h'),
                    mode: KeyMod::CTRL,
                }])],
                name: "sweep.help".to_owned(),
                description: "Show help menu".to_owned(),
            },
            ScorerNext => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('s'),
                    mode: KeyMod::CTRL,
                }])],
                name: SWEEP_SCORER_NEXT_TAG.to_owned(),
                description: "Switch to next available scorer".to_owned(),
            },
            PreviewToggle => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('p'),
                    mode: KeyMod::ALT,
                }])],
                name: "sweep.preview.toggle".to_owned(),
                description: "Toggle preview for an item".to_owned(),
            },
            PreviewLineNext => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('j'),
                    mode: KeyMod::ALT,
                }])],
                name: "sweep.preview.line.next".to_owned(),
                description: "Scroll preview one line down".to_owned(),
            },
            PreviewLinePrev => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::ALT,
                }])],
                name: "sweep.preview.line.prev".to_owned(),
                description: "Scroll preview one line up".to_owned(),
            },
            Input(input_action) => input_action.description(),
            List(list_action) => list_action.description(),
        }
    }

    fn all() -> impl Iterator<Item = SweepAction> {
        use SweepAction::*;
        [
            Select,
            Mark,
            MarkAll,
            Quit,
            Help,
            ScorerNext,
            PreviewToggle,
            PreviewLineNext,
            PreviewLinePrev,
        ]
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
    // footer
    footer: Option<Arc<dyn View>>,
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
    // marked items (multi-select)
    marked: Arc<RwLock<MarkedItems<H>>>,
    // ranker
    ranker: Ranker<H>,
    // haystack context
    haystack_context: H::Context,
    // cached large preview of the current item
    preview_large: Option<SweepPreview<H::PreviewLarge>>,
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
    fn new_from_options(
        options: &SweepOptions,
        waker: TerminalWaker,
        haystack_context: H::Context,
    ) -> Self {
        let ranker = Ranker::new(haystack_context.clone(), move |_| waker.wake().is_ok());
        ranker.scorer_set(options.scorers[0].clone());
        ranker.keep_order(Some(options.keep_order));
        SweepState::new(
            options.prompt.clone(),
            options.prompt_icon.clone(),
            ranker,
            options.theme.clone(),
            options.scorers.clone(),
            haystack_context,
        )
    }

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
                key_map.register(chord, action.clone());
            }
        }

        // widgets
        let input = Input::new(theme.clone());
        let list = List::new(
            SweepItems::new(
                Arc::new(RankedItems::<H>::default()),
                Default::default(),
                haystack_context.clone(),
            ),
            theme.clone(),
        );

        Self {
            scorers,
            prompt,
            prompt_icon,
            footer: None,
            key_map_state: Vec::new(),
            key_empty_backspace: None,
            key_map,
            key_actions,
            theme,
            input,
            list,
            marked: Default::default(),
            ranker,
            haystack_context,
            preview_large: None,
        }
    }

    // get preview of the currently pointed haystack item
    fn preview(&self) -> Option<H::Preview> {
        let item = self.list.current()?;
        item.item.haystack.preview(
            &self.haystack_context,
            item.item.positions.as_ref(),
            &self.theme,
        )
    }

    // get large preview for currently pointed haystack item
    fn preview_large(&mut self) -> Option<SweepPreview<H::PreviewLarge>> {
        let item = self.list.current()?;
        if !matches!(&self.preview_large, Some(preview) if preview.id == item.item.id) {
            let preview = item.item.haystack.preview_large(
                &self.haystack_context,
                item.item.positions.as_ref(),
                &self.theme,
            )?;
            self.preview_large = Some(SweepPreview::new(item.item.id, self.theme.clone(), preview));
        }
        self.preview_large.clone()
    }

    // update theme
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

    /// Trigger ranker, should be called whenever needle might have changed
    fn ranker_trigger(&self) {
        // ranker only runs if needle has actually been updated, so it is safe
        // to run whenever needle might have changed
        self.ranker.needle_set(self.input.get().collect());
    }

    /// Retrieve latest ranker result and update list view
    fn ranker_refresh(&mut self) -> Arc<RankedItems<H>> {
        // check if list view needs to be updated
        let ranker_result = self.ranker.result();
        if self.list.items().generation() != ranker_result.generation() {
            // find cursor position of currently pointed item in the new result
            let cursor = if self.list.cursor() == 0 {
                None
            } else {
                self.list
                    .items()
                    .ranked_items
                    .get_haystack_index(self.list.cursor())
                    .and_then(|haystack_index| ranker_result.find_match_index(haystack_index))
            };
            // update list with new results
            let old_items = self.list.items_set(SweepItems::new(
                ranker_result.clone(),
                self.marked.clone(),
                self.haystack_context.clone(),
            ));
            if let Some(cursor) = cursor {
                self.list.cursor_set(cursor);
            }
            // dropping old result might add noticeable delay for large lists
            rayon::spawn(move || std::mem::drop(old_items));
        }
        ranker_result
    }

    fn apply(&mut self, action: &SweepAction) -> SweepKeyEvent<H> {
        use SweepKeyEvent::*;
        match action {
            SweepAction::Input(action) => {
                self.input.apply(action);
                self.ranker_trigger();
            }
            SweepAction::List(action) => self.list.apply(action),
            SweepAction::User { tag, chord, .. } => {
                if !tag.is_empty() {
                    return Event(SweepEvent::Bind {
                        tag: tag.clone(),
                        chord: chord.clone(),
                    });
                }
            }
            SweepAction::Quit => {
                return SweepKeyEvent::Quit;
            }
            SweepAction::Select => {
                if !self.marked.with(|marked| marked.is_empty()) {
                    let marked = self.marked.with_mut(|marked| marked.take()).collect();
                    return Event(SweepEvent::Select(marked));
                }
                if let Some(current) = self.list.current() {
                    return Event(SweepEvent::Select(vec![current.item.haystack]));
                } else {
                    return Event(SweepEvent::Select(Vec::new()));
                }
            }
            SweepAction::Mark => {
                if let Some(current) = self.list.current() {
                    self.marked.with_mut(|marked| marked.toggle(current.item));
                    self.list.apply(&ListAction::ItemNext);
                }
            }
            SweepAction::MarkAll => {
                self.marked.with_mut(|marked| {
                    if marked.is_empty() {
                        // mark all
                        for item in self.list.items().ranked_items.iter() {
                            marked.toggle(item)
                        }
                    } else {
                        // un-mark all
                        _ = marked.take();
                    }
                })
            }
            SweepAction::Help => return Help,
            SweepAction::ScorerNext => {
                self.scorer_by_name(None);
                return Nothing;
            }
            SweepAction::PreviewToggle => self.theme_set(
                self.theme
                    .modify(|inner| inner.show_preview = !self.theme.show_preview),
            ),
            SweepAction::PreviewLineNext => {
                if let Some(preview) = self.preview_large.as_ref() {
                    let layout = preview.preview.preview_layout();
                    let mut offset = layout.position();
                    offset.row = layout.size().height.min(offset.row + 1);
                    preview.preview.set_offset(offset);
                }
            }
            SweepAction::PreviewLinePrev => {
                if let Some(preview) = self.preview_large.as_ref() {
                    let layout = preview.preview.preview_layout();
                    let mut offset = layout.position();
                    offset.row = offset.row.saturating_sub(1);
                    preview.preview.set_offset(offset);
                }
            }
        }
        Nothing
    }

    fn handle_key(&mut self, key: Key) -> SweepKeyEvent<H> {
        use SweepKeyEvent::*;
        let is_first_key = self.key_map_state.is_empty();
        if let Some(action) = self.key_map.lookup_state(&mut self.key_map_state, key) {
            tracing::debug!(?action, "[SweepState.handle_key]");
            // do not generate Backspace, when input is not empty
            let backspace = Key::new(KeyName::Backspace, KeyMod::EMPTY);
            if is_first_key && key == backspace && self.input.get().count() == 0 {
                if let Some(ref tag) = self.key_empty_backspace {
                    return Event(SweepEvent::Bind {
                        tag: tag.clone(),
                        chord: KeyChord::from_iter([backspace]),
                    });
                }
            } else {
                return self.apply(&action.clone());
            }
        } else if let Key {
            name: KeyName::Char(c),
            mode: KeyMod::EMPTY,
        } = key
        {
            // send plain chars to the input
            self.input.apply(&InputAction::Insert(c));
            self.ranker_trigger();
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
                .and_modify(|desc_curr| desc_curr.chords.push(KeyChord::from_iter(chord)))
                .or_insert_with(|| {
                    desc.chords.clear();
                    desc.chords.push(KeyChord::from_iter(chord));
                    desc
                });
        });
        let mut entries: Vec<_> = descriptions.into_values().collect();
        entries.sort_by_key(|desc| self.key_actions.get(&desc.name));

        let ranker = Ranker::new((), move |_| term_waker.wake().is_ok());
        ranker.keep_order(Some(true));
        ranker.haystack_extend(entries);
        SweepState::new(
            "BINDINGS".to_owned(),
            Some(KEYBOARD_ICON.clone()),
            ranker,
            self.theme.modify(|inner| inner.show_preview = true),
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
        let mut stats = Text::new()
            .put_text(&self.theme.separator_left)
            .with_face(self.theme.stats)
            .with_char(' ')
            .take();
        let marked_count = self.marked.with(|marked| marked.len());
        if marked_count > 0 {
            stats.put_fmt(&format_args!("{}/", marked_count), None);
        }
        stats
            .put_fmt(
                &format_args!("{}/{} ", ranker_result.len(), ranker_result.haystack_len(),),
                None,
            )
            .put_fmt(&format_args!("{:.0?}", ranker_result.duration()), None)
            .scope(|text| {
                let name = ranker_result.scorer().name();
                match ICONS.get(name) {
                    Some(glyph) => {
                        text.put_glyph(glyph.clone());
                    }
                    None => {
                        text.put_fmt(name, None);
                    }
                };
            });

        // prompt
        let prompt = Text::new()
            .with_face(self.theme.label)
            .scope(|text| {
                match &self.prompt_icon {
                    Some(icon) => text.put_glyph(icon.clone()),
                    None => text.put_char(' '),
                };
            })
            .put_fmt(&self.prompt, None)
            .with_char(' ')
            .put_text(&self.theme.separator_right)
            .take();

        // header
        let header = Flex::row()
            .add_child(prompt)
            .add_flex_child(1.0, &self.input)
            .add_child(stats.tag(Value::String(SWEEP_SCORER_NEXT_TAG.to_string())));

        // list
        let mut body = Flex::row();
        body.push_flex_child(1.0, &self.list);
        // preview
        if self.theme.show_preview {
            if let Some(preview) = self.preview() {
                let flex = preview.flex().unwrap_or(0.0);
                let mut view = Container::new(preview)
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

        let mut view = Flex::column()
            .add_child(Container::new(header).with_height(1))
            .add_flex_child(1.0, body);
        if let Some(footer) = &self.footer {
            view.push_child_ext(
                footer.clone(),
                None,
                Some(self.theme.list_default),
                Align::Expand,
            )
        }
        view
    }
}

fn sweep_ui_worker<H>(
    mut options: SweepOptions,
    mut term: SystemTerminal,
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
    term.execute_many(TerminalCommand::mouse_events_set(true, false))?;
    // force dumb four color theme for dumb terminal
    if ColorDepth::Gray == term.capabilities().depth {
        options.theme = Theme::dumb().modify(|inner| inner.show_preview = true);
    }

    // prepare terminal based on layout
    let term_size = term.size()?;
    let mut win_layout = options
        .layout
        .compute(term.position()?, term_size.cells, false);
    if win_layout.position().row + win_layout.size().height > term_size.cells.height {
        // scroll to reserve space
        let scroll = win_layout.position().row + win_layout.size().height - term_size.cells.height;
        let row = term_size.cells.height - win_layout.size().height;
        win_layout.set_position(Position {
            row,
            ..win_layout.position()
        });
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }
    if options.layout.is_altscreen() {
        term.execute(TerminalCommand::altscreen_set(true))?;
    }
    // report calculated size
    events.send(SweepEvent::Resize(TerminalSize {
        cells: win_layout.size(),
        pixels: term_size.cells_in_pixels(win_layout.size()),
    }))?;

    // sweep state
    let mut state = SweepState::new_from_options(&options, term.waker(), haystack_context.clone());
    let mut state_stack: Vec<SweepState<H>> = Vec::new();
    let mut state_help: Option<SweepState<ActionDesc>> = None;

    // render loop
    let mut render_suppress = false;
    let mut render_supress_sync: Option<Arc<AtomicBool>> = None;
    let mut layout_store = ViewLayoutStore::new();
    let mut layout_id: Option<TreeId> = None;
    term.waker().wake()?; // schedule one wake just in case if it was consumed by previous poll
    let result = term.run_render(|term, event, surf| {
        // handle events
        match event {
            Some(TerminalEvent::Resize(term_size)) => {
                term.execute(TerminalCommand::Face(Default::default()))?;
                term.execute(TerminalCommand::EraseScreen)?;
                win_layout = options
                    .layout
                    .compute(win_layout.position(), term_size.cells, true);
                events.send(SweepEvent::Resize(TerminalSize {
                    cells: win_layout.size(),
                    pixels: term_size.cells_in_pixels(win_layout.size()),
                }))?;
            }
            Some(TerminalEvent::Wake) => {
                for request in requests.try_iter() {
                    use SweepRequest::*;
                    match request {
                        NeedleSet(needle) => {
                            state.input.set(needle.as_ref());
                            state.ranker_trigger();
                        }
                        NeedleGet(resolve) => {
                            mem::drop(resolve.send(state.input.get().collect()));
                        }
                        ThemeGet(resolve) => {
                            mem::drop(resolve.send(state.theme.clone()));
                        }
                        Terminate => return Ok(TerminalAction::Quit(())),
                        Bind { chord, tag, desc } => match *chord.keys() {
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
                                        chord: KeyChord::new(Vec::new()),
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
                            let current = state.list.current().map(|item| item.item.haystack);
                            _ = resolve.send(current);
                        }
                        Marked(resolve) => {
                            let items = state.marked.with_mut(|marked| marked.take()).collect();
                            _ = resolve.send(items);
                        }
                        CursorSet { position } => {
                            state.list.cursor_set(position);
                        }
                        ScorerByName(name, resolve) => {
                            let _ = resolve.send(state.scorer_by_name(name));
                        }
                        PreviewSet(value) => {
                            let show_preview = match value {
                                Some(value) => value,
                                None => !state.theme.show_preview,
                            };
                            state.theme_set(
                                state
                                    .theme
                                    .modify(|inner| inner.show_preview = show_preview),
                            );
                        }
                        FooterSet(view) => state.footer = view,
                        ScorerSet(scorer) => state.ranker.scorer_set(scorer),
                        HaystackExtend(items) => state.ranker.haystack_extend(items),
                        HaystackUpdate { index, item } => state.ranker.haystack_update(index, item),
                        HaystackClear => state.ranker.haystack_clear(),
                        HaystackReverse => state.ranker.haystack_reverse(),
                        RankerKeepOrder(toggle) => state.ranker.keep_order(toggle),
                        StatePush => {
                            let state_old = std::mem::replace(
                                &mut state,
                                SweepState::new_from_options(
                                    &options,
                                    term.waker(),
                                    haystack_context.clone(),
                                ),
                            );
                            state_stack.push(state_old);
                        }
                        StatePop => {
                            if let Some(state_new) = state_stack.pop() {
                                state = state_new;
                            }
                        }
                        RenderSuppress(suppress) => {
                            render_suppress = suppress;
                            if !suppress {
                                render_supress_sync = Some(state.ranker.sync());
                            }
                            tracing::debug!(?suppress, "[sweep_ui_worker][render_suppress]");
                        }
                    }
                }
            }
            Some(TerminalEvent::Key(key)) => {
                let key_event = match state_help.as_mut() {
                    None => state.handle_key(key),
                    Some(help) => match help.handle_key(key) {
                        SweepKeyEvent::Quit => {
                            state_help.take();
                            SweepKeyEvent::Nothing
                        }
                        SweepKeyEvent::Event(SweepEvent::Select(actions_descs)) => {
                            state_help.take();
                            if let Some(action) = actions_descs
                                .first()
                                .and_then(|action_desc| state.key_actions.get(&action_desc.name))
                            {
                                state.apply(&action.clone())
                            } else {
                                SweepKeyEvent::Nothing
                            }
                        }
                        _ => SweepKeyEvent::Nothing,
                    },
                };
                match key_event {
                    SweepKeyEvent::Event(event) => {
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
            Some(TerminalEvent::Mouse(mouse)) => {
                if let Some(layout) = layout_id.map(|id| TreeView::from_id(&layout_store, id)) {
                    // adjust mouse position to account for window offset
                    let win_pos = win_layout.position();
                    if mouse.pos.col >= win_pos.col && mouse.pos.row >= win_pos.row {
                        let pos = Position {
                            row: mouse.pos.row - win_pos.row,
                            col: mouse.pos.col - win_pos.col,
                        };
                        let mut tag: Option<&Value> = None;
                        for layout in layout.find_path(pos) {
                            if let Some(tag_next) = layout.data::<Value>() {
                                tag = Some(tag_next);
                            };
                        }
                        tracing::debug!(?tag, ?mouse, "[sweep_ui_worker][mouse]");
                        if let Some(tag) = tag.and_then(|tag| tag.as_str()) {
                            let key_event = match state.key_actions.get(tag) {
                                // trigger state bound actions on release
                                Some(action) if mouse.mode == KeyMod::EMPTY => {
                                    state.apply(&action.clone())
                                }
                                // ignore press events, and trigger on release
                                _ if mouse.mode.contains(KeyMod::PRESS) => SweepKeyEvent::Nothing,
                                _ => {
                                    let key = Key::new(mouse.name, mouse.mode);
                                    SweepKeyEvent::Event(SweepEvent::Bind {
                                        tag: tag.to_owned(),
                                        chord: KeyChord::from_iter([key]),
                                    })
                                }
                            };
                            match key_event {
                                SweepKeyEvent::Event(event) => {
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
                    }
                }
            }
            _ => (),
        }

        // render
        let mut win_surf = win_layout.apply_to(surf);
        let ctx = ViewContext::new(term)?;
        let layout_id_next = if let Some(state_help) = state_help.as_mut() {
            state_help.ranker_refresh();
            tracing::debug_span!("[sweep_ui_worker][draw] sweep help state")
                .in_scope(|| win_surf.draw_view(&ctx, Some(&mut layout_store), state_help))?
        } else {
            if render_suppress
                || !render_supress_sync
                    .as_ref()
                    .map_or_else(|| true, |s| s.load(Ordering::Acquire))
            {
                tracing::debug!("[sweep_ui_worker][draw] suppressed");
                return Ok(TerminalAction::WaitNoFrame);
            }
            render_supress_sync.take();
            state.ranker_refresh();

            let view = if let SweepLayout::Full { height } = &options.layout {
                if let Some(preview_large) = state.preview_large() {
                    let main_height = height.calc(win_layout.size().height);
                    let main = Container::new(&mut state);
                    let mut flex = Flex::column();
                    if height.is_positive() {
                        flex.push_child(main.with_height(main_height));
                        flex.push_flex_child(1.0, preview_large);
                    } else {
                        flex.push_flex_child(1.0, preview_large);
                        flex.push_child(main.with_height(win_layout.size().height - main_height));
                    }
                    flex.left_view()
                } else {
                    state.into_view().right_view()
                }
            } else {
                state.into_view().right_view()
            };

            tracing::debug_span!("[sweep_ui_worker][draw] sweep state")
                .in_scope(|| win_surf.draw_view(&ctx, Some(&mut layout_store), view))?
        };
        layout_id = Some(layout_id_next);

        Ok(TerminalAction::Wait)
    });

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: win_layout.position().row,
        col: 0,
    }))?;
    if options.layout.is_altscreen() {
        term.execute(TerminalCommand::altscreen_set(false))?;
    }
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    result
}

struct SweepItems<H: Haystack> {
    ranked_items: Arc<RankedItems<H>>,
    marked_items: Arc<RwLock<MarkedItems<H>>>,
    haystack_context: H::Context,
}

impl<H: Haystack> SweepItems<H> {
    fn new(
        ranked_items: Arc<RankedItems<H>>,
        marked_items: Arc<RwLock<MarkedItems<H>>>,
        haystack_context: H::Context,
    ) -> Self {
        Self {
            ranked_items,
            marked_items,
            haystack_context,
        }
    }

    fn generation(&self) -> usize {
        self.ranked_items.generation()
    }
}

impl<H: Haystack> ListItems for SweepItems<H> {
    type Item = SweepItem<H>;

    fn len(&self) -> usize {
        self.ranked_items.len()
    }

    fn get(&self, index: usize, theme: Theme) -> Option<Self::Item> {
        self.ranked_items.get(index).map(|item| SweepItem {
            item: item.clone(),
            theme,
            haystack_context: self.haystack_context.clone(),
        })
    }

    fn is_marked(&self, item: &Self::Item) -> bool {
        self.marked_items
            .with(|marked| marked.contains_id(item.item.id))
    }
}

struct SweepItem<H: Haystack> {
    item: RankedItem<H>,
    theme: Theme,
    haystack_context: H::Context,
}

impl<H: Haystack> IntoView for SweepItem<H> {
    type View = H::View;

    fn into_view(self) -> Self::View {
        self.item.haystack.view(
            &self.haystack_context,
            self.item.positions.as_ref(),
            &self.theme,
        )
    }
}

/// Set of marked (multi-selected) items
struct MarkedItems<H> {
    items: BTreeMap<usize, H>,
    ids: HashMap<RankedItemId, usize>,
    index: usize,
}

impl<H> Default for MarkedItems<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> MarkedItems<H> {
    fn new() -> Self {
        Self {
            items: Default::default(),
            ids: Default::default(),
            index: 0,
        }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn toggle(&mut self, item: RankedItem<H>) {
        let id = item.id;
        match self.ids.get(&id) {
            Some(index) => {
                self.items.remove(index);
                self.ids.remove(&id);
            }
            None => {
                self.ids.insert(id, self.index);
                self.items.insert(self.index, item.haystack);
                self.index += 1;
            }
        }
    }

    fn take(&mut self) -> impl Iterator<Item = H> {
        self.ids.clear();
        std::mem::take(&mut self.items).into_values()
    }

    fn contains_id(&self, id: RankedItemId) -> bool {
        self.ids.contains_key(&id)
    }
}

#[derive(Debug, Clone)]
pub enum SweepLayout {
    Float {
        height: SweepLayoutSize,
        width: SweepLayoutSize,
        row: SweepLayoutSize,
        column: SweepLayoutSize,
    },
    Full {
        height: SweepLayoutSize,
    },
}

impl SweepLayout {
    fn is_altscreen(&self) -> bool {
        matches!(self, SweepLayout::Full { .. })
    }

    fn compute(&self, term_pos: Position, term_size: Size, row_limit: bool) -> Layout {
        match self {
            SweepLayout::Float {
                height,
                width,
                row,
                column,
            } => {
                let mut pos = term_pos;
                if !row.is_full() {
                    pos.row = row.calc(term_size.height);
                }
                pos.col = column.calc(term_size.width);
                let size = Size {
                    height: height.calc(term_size.height),
                    width: width.calc(term_size.width).min(term_size.width - pos.col),
                };
                if row_limit {
                    pos.row = pos.row.min(term_size.height - size.height);
                }
                Layout::new().with_position(pos).with_size(size)
            }
            SweepLayout::Full { .. } => Layout::new().with_size(term_size),
        }
    }
}

impl Default for SweepLayout {
    fn default() -> Self {
        use SweepLayoutSize::*;
        SweepLayout::Float {
            height: Absolute(11),
            width: Full,
            column: Absolute(0),
            row: Full,
        }
    }
}

impl std::str::FromStr for SweepLayout {
    type Err = Error;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        // name(,attr=value)*
        let mut iter = string.trim().split(',');
        let Some(name) = iter.next() else {
            anyhow::bail!("invalid layout: {} (expected `name(,attr=value)`)", string);
        };
        let kvs = iter.filter_map(|kv| {
            let mut kv = kv.splitn(2, '=');
            let key = kv.next()?.trim();
            let value = kv.next()?.trim();
            Some((key, value))
        });
        match name {
            "float" => {
                let mut height = SweepLayoutSize::Absolute(11);
                let mut width = SweepLayoutSize::Full;
                let mut column = SweepLayoutSize::Absolute(0);
                let mut row = SweepLayoutSize::Full;
                for (key, value) in kvs {
                    match key {
                        "height" | "h" => height = value.parse()?,
                        "width" | "w" => width = value.parse()?,
                        "column" | "c" => column = value.parse()?,
                        "row" | "r" => row = value.parse()?,
                        _ => {}
                    }
                }
                Ok(SweepLayout::Float {
                    height,
                    width,
                    row,
                    column,
                })
            }
            "full" => {
                let mut height = SweepLayoutSize::Full;
                for (key, value) in kvs {
                    match key {
                        "height" | "h" => height = value.parse()?,
                        _ => {}
                    }
                }
                Ok(SweepLayout::Full { height })
            }
            _ => Err(anyhow::anyhow!("invalid layout name: {}", name)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SweepLayoutSize {
    Absolute(i32),
    Fraction(f32),
    Full,
}

impl SweepLayoutSize {
    fn calc(&self, size: usize) -> usize {
        match *self {
            SweepLayoutSize::Absolute(diff) => {
                if diff >= 0 {
                    (diff as usize).clamp(0, size)
                } else {
                    size - (-diff as usize).clamp(0, size)
                }
            }
            SweepLayoutSize::Fraction(frac) => {
                if frac >= 0.0 {
                    ((size as f32 * frac) as usize).clamp(0, size)
                } else {
                    size - ((size as f32 * -frac) as usize).clamp(0, size)
                }
            }
            SweepLayoutSize::Full => size,
        }
    }

    fn is_full(&self) -> bool {
        matches!(self, SweepLayoutSize::Full)
    }

    fn is_positive(&self) -> bool {
        match *self {
            SweepLayoutSize::Absolute(val) => val >= 0,
            SweepLayoutSize::Fraction(val) => val >= 0.0,
            SweepLayoutSize::Full => true,
        }
    }
}

impl std::str::FromStr for SweepLayoutSize {
    type Err = Error;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        let string = string.trim();
        if matches!(string, "full" | "dynamic") {
            Ok(Self::Full)
        } else if let Some(perc) = string.strip_suffix('%') {
            let perc: f32 = perc.parse()?;
            Ok(Self::Fraction((perc / 100.0).clamp(-1.0, 1.0)))
        } else {
            Ok(Self::Absolute(string.parse()?))
        }
    }
}

#[derive(Clone)]
struct SweepPreview<P> {
    id: RankedItemId,
    theme: Theme,
    preview: P,
    height: Arc<AtomicUsize>,
}

impl<P> SweepPreview<P> {
    fn new(id: RankedItemId, theme: Theme, preview: P) -> Self {
        Self {
            id,
            preview,
            theme,
            height: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl<P> IntoView for SweepPreview<P>
where
    P: HaystackPreview + Clone + 'static,
{
    type View = Flex<'static>;

    fn into_view(self) -> Self::View {
        let preview = self.preview.clone().trace_layout({
            let height = self.height.clone();
            move |_, layout| {
                height.store(layout.size().height, Ordering::Relaxed);
            }
        });
        let scrollbar = ScrollBarFn::new(
            Axis::Vertical,
            Face::new(Some(self.theme.accent), None, FaceAttrs::default()),
            {
                let layout = self.preview.preview_layout();
                let height = self.height.clone();
                move || {
                    ScrollBarPosition::from_counts(
                        layout.size().height,
                        layout.position().row,
                        height.load(Ordering::Relaxed),
                    )
                }
            },
        );
        let view = Flex::row()
            .add_flex_child(1.0, preview)
            .add_child(scrollbar);
        view
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
