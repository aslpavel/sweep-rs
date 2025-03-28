use crate::{
    ALL_SCORER_BUILDERS, Haystack, HaystackPreview, RankedItems, Ranker, RankerThread, ScoreItem,
    ScorerBuilder,
    common::{LockExt, VecDeserializeSeed},
    rpc::{RpcError, RpcParams, RpcPeer},
    scorer_by_name,
    widgets::{ActionDesc, Input, InputAction, List, ListAction, ListItems, Theme},
};
use anyhow::{Context, Error};
use crossbeam_channel::{Receiver, Sender, unbounded};
use either::Either;
use futures::{Stream, channel::oneshot, future, stream::TryStreamExt};
use serde::{
    Deserialize, Serialize,
    de::{DeserializeOwned, DeserializeSeed},
};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    future::Future,
    marker::PhantomData,
    mem,
    ops::Deref,
    sync::{
        Arc, LazyLock, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{Builder, JoinHandle},
    time::Duration,
};
use surf_n_term::{
    CellWrite, Face, FaceAttrs, Glyph, Key, KeyChord, KeyMap, KeyMapHandler, KeyMod, KeyName,
    Position, Size, SystemTerminal, Terminal, TerminalAction, TerminalCommand, TerminalEvent,
    TerminalSize, TerminalSurface, TerminalSurfaceExt, TerminalWaker,
    encoder::ColorDepth,
    terminal::Mouse,
    view::{
        Align, Axis, BoxConstraint, BoxView, Container, Flex, FlexChild, FlexRef, IntoView,
        Margins, ScrollBarFn, ScrollBarPosition, Text, Tree, TreeId, TreeMut, TreeView, View,
        ViewCache, ViewContext, ViewDeserializer, ViewLayout, ViewLayoutStore, ViewMutLayout,
    },
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Mutex, mpsc},
};

static ICONS: LazyLock<HashMap<String, Glyph>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("./icons.json")).expect("invalid icons.json file")
});
pub static PROMPT_DEFAULT_ICON: LazyLock<&'static Glyph> = LazyLock::new(|| {
    ICONS
        .get("prompt")
        .expect("failed to get prompt default icon")
});
static KEYBOARD_ICON: LazyLock<&'static Glyph> =
    LazyLock::new(|| ICONS.get("keyboard").expect("failed to get keyboard icon"));
const SWEEP_SCORER_NEXT_TAG: &str = "sweep.scorer.next";

#[derive(Clone)]
pub struct SweepOptions {
    pub prompt: String,
    pub prompt_icon: Option<Glyph>,
    pub keep_order: bool,
    pub scorers: VecDeque<ScorerBuilder>,
    pub theme: Theme,
    pub title: String,
    pub tty_path: String,
    pub layout: WindowLayout,
    /// default window id, if None no default window is created
    pub window_uid: Option<WindowId>,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            prompt: "INPUT".to_string(),
            prompt_icon: Some(PROMPT_DEFAULT_ICON.clone()),
            theme: Theme::light(),
            keep_order: false,
            tty_path: "/dev/tty".to_string(),
            title: "sweep".to_string(),
            window_uid: Some(WindowId::String("default".into())),
            scorers: ALL_SCORER_BUILDERS.clone(),
            layout: WindowLayout::default(),
        }
    }
}

impl fmt::Debug for SweepOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scorers: Vec<_> = self
            .scorers
            .iter()
            .map(|builder| builder("").name().to_owned())
            .collect();
        f.debug_struct("SweepOptions")
            .field("prompt", &self.prompt)
            .field("prompt_icon", &self.prompt_icon)
            .field("keep_order", &self.keep_order)
            .field("scorers", &scorers)
            .field("theme", &self.theme)
            .field("title", &self.title)
            .field("tty_path", &self.tty_path)
            .field("layout", &self.layout)
            .finish()
    }
}

/// Simple sweep function when you just need to select single entry from the stream of items
pub async fn sweep<IS, I, E>(
    options: SweepOptions,
    items_context: I::Context,
    items: IS,
) -> Result<Vec<I>, Error>
where
    IS: Stream<Item = Result<I, E>>,
    I: Haystack,
    Error: From<E>,
{
    let sweep: Sweep<I> = Sweep::new(items_context, options)?;
    let collect = sweep.items_extend_stream(None, items.map_err(Error::from));
    let mut collected = false; // whether all items are send sweep instance
    tokio::pin!(collect);
    loop {
        tokio::select! {
            event = sweep.next_event() => match event {
                Some(SweepEvent::Select { items, .. }) => return Ok(items),
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

enum SweepWindowRequest<H> {
    NeedleSet(String),
    NeedleGet(oneshot::Sender<String>),
    PromptSet(Option<String>, Option<Glyph>),
    ThemeGet(oneshot::Sender<Theme>),
    Bind {
        chord: KeyChord,
        tag: String,
        desc: String,
    },
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
    RankerKeepOrder(Option<bool>),
    RenderSuppress(bool),
}

/// Request generated by [Sweep] type
enum SweepRequest<H> {
    Terminate,
    WindowSwitch {
        window: Either<Box<dyn Window>, WindowId>,
        close: bool,
        created: oneshot::Sender<bool>,
    },
    WindowPop,
    WindowRequest {
        uid: Option<WindowId>,
        request: SweepWindowRequest<H>,
    },
}

/// Events returned to [Sweep] type
#[derive(Clone, Debug)]
pub enum SweepEvent<H> {
    Select {
        uid: WindowId,
        items: Vec<H>,
    },
    Bind {
        uid: WindowId,
        tag: Arc<str>,
        chord: KeyChord,
    },
    Window(WindowEvent),
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

    fn send_window_request(&self, uid: Option<WindowId>, request: SweepWindowRequest<H>) {
        self.requests
            .send(SweepRequest::WindowRequest { uid, request })
            .expect("failed to send request to sweep_worker");
        self.term_waker.wake().expect("failed to wake terminal");
    }

    /// Get terminal waker
    pub fn waker(&self) -> TerminalWaker {
        self.term_waker.clone()
    }

    /// Toggle preview associated with the current item
    pub fn preview_set(self, uid: Option<WindowId>, value: Option<bool>) {
        self.send_window_request(uid, SweepWindowRequest::PreviewSet(value));
    }

    /// Extend list of searchable items from iterator
    pub fn items_extend<HS>(&self, uid: Option<WindowId>, items: HS)
    where
        HS: IntoIterator,
        H: From<HS::Item>,
    {
        let items = items.into_iter().map(From::from).collect();
        self.send_window_request(uid, SweepWindowRequest::HaystackExtend(items))
    }

    /// Extend list of searchable items from stream
    pub async fn items_extend_stream(
        &self,
        uid: Option<WindowId>,
        items: impl Stream<Item = Result<H, Error>>,
    ) -> Result<(), Error> {
        items
            .try_chunks(1024)
            .map_err(|e| e.1)
            .try_for_each(|chunk| {
                let uid = uid.clone();
                async move {
                    self.items_extend(uid.clone(), chunk);
                    Ok(())
                }
            })
            .await
    }

    /// Update item by its index
    pub fn item_update(&self, uid: Option<WindowId>, index: usize, item: H) {
        self.send_window_request(uid, SweepWindowRequest::HaystackUpdate { index, item })
    }

    /// Clear list of searchable items
    pub fn items_clear(&self, uid: Option<WindowId>) {
        self.send_window_request(uid, SweepWindowRequest::HaystackClear)
    }

    /// Get currently selected items
    pub async fn items_current(&self, uid: Option<WindowId>) -> Result<Option<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_window_request(uid, SweepWindowRequest::Current(send));
        recv.await.context("items_current")
    }

    /// Get marked (multi-select) items
    pub async fn items_marked(&self, uid: Option<WindowId>) -> Result<Vec<H>, Error> {
        let (send, recv) = oneshot::channel();
        self.send_window_request(uid, SweepWindowRequest::Marked(send));
        recv.await.context("items_marked")
    }

    /// Set needle to the specified string
    pub fn query_set(&self, uid: Option<WindowId>, needle: impl AsRef<str>) {
        self.send_window_request(
            uid,
            SweepWindowRequest::NeedleSet(needle.as_ref().to_string()),
        )
    }

    /// Get current needle value
    pub async fn query_get(&self, uid: Option<WindowId>) -> Result<String, Error> {
        let (send, recv) = oneshot::channel();
        self.send_window_request(uid, SweepWindowRequest::NeedleGet(send));
        recv.await.context("query_get")
    }

    /// Set scorer used for ranking
    pub fn scorer_set(&self, uid: Option<WindowId>, scorer: ScorerBuilder) {
        self.send_window_request(uid, SweepWindowRequest::ScorerSet(scorer))
    }

    /// Whether to keep order of elements or not
    pub fn keep_order(&self, uid: Option<WindowId>, toggle: Option<bool>) {
        self.send_window_request(uid, SweepWindowRequest::RankerKeepOrder(toggle))
    }

    /// Switch scorer, if name is not provided next scorer is chosen
    pub async fn scorer_by_name(
        &self,
        uid: Option<WindowId>,
        name: Option<String>,
    ) -> Result<(), Error> {
        let (send, recv) = oneshot::channel();
        self.send_window_request(uid, SweepWindowRequest::ScorerByName(name.clone(), send));
        if !recv.await.context("scorer_by_name")? {
            return Err(anyhow::anyhow!("unkown scorer type: {:?}", name));
        }
        Ok(())
    }

    /// Set prompt
    pub fn prompt_set(&self, uid: Option<WindowId>, prompt: Option<String>, icon: Option<Glyph>) {
        self.send_window_request(uid, SweepWindowRequest::PromptSet(prompt, icon))
    }

    /// Get current theme
    pub async fn theme_get(&self, uid: Option<WindowId>) -> Result<Theme, Error> {
        let (send, recv) = oneshot::channel();
        self.send_window_request(uid, SweepWindowRequest::ThemeGet(send));
        recv.await.context("theme_get")
    }

    /// Set footer
    pub fn footer_set(&self, uid: Option<WindowId>, footer: Option<Arc<dyn View>>) {
        self.send_window_request(uid, SweepWindowRequest::FooterSet(footer))
    }

    /// Set cursor to specified position
    pub fn cursor_set(&self, uid: Option<WindowId>, position: usize) {
        self.send_window_request(uid, SweepWindowRequest::CursorSet { position })
    }

    /// Bind specified chord to the tag
    ///
    /// Whenever sequence of keys specified by chord is pressed, [SweepEvent::Bind]
    /// will be generated, note if tag is empty string the binding will be removed
    /// and no event will be generated. Tag can also be a standard action name
    /// (see available with `ctrl+h`) in this case [SweepEvent::Bind] is not generated.
    pub fn bind(&self, uid: Option<WindowId>, chord: KeyChord, tag: String, desc: String) {
        self.send_window_request(uid, SweepWindowRequest::Bind { chord, tag, desc })
    }

    /// Suppress rendering to reduce flickering
    pub fn render_suppress(&self, uid: Option<WindowId>, suppress: bool) {
        self.send_window_request(uid, SweepWindowRequest::RenderSuppress(suppress))
    }

    fn send_request(&self, request: SweepRequest<H>) {
        self.requests
            .send(request)
            .expect("failed to send request to sweep_worker");
        self.term_waker.wake().expect("failed to wake terminal");
    }

    /// Create new state and put on the top of the stack of active states
    pub async fn window_switch(&self, uid: WindowId, close: bool) -> Result<bool, Error> {
        let (send, recv) = oneshot::channel();
        self.send_request(SweepRequest::WindowSwitch {
            window: Either::Right(uid),
            created: send,
            close,
        });
        recv.await.context("window_switch")
    }

    /// Remove state at the top of the stack and active one below it
    pub fn window_pop(&self) {
        self.send_request(SweepRequest::WindowPop)
    }

    /// Select from the list of items
    ///
    /// Useful when you only need to select from few items (i.e Y/N) and the type
    /// of items is not necessarily equal to the type of the [Sweep] instance items
    pub async fn quick_select<I>(
        &self,
        options: Option<SweepOptions>,
        uid: WindowId,
        items_context: <I::Item as Haystack>::Context,
        items: I,
    ) -> Result<Vec<I::Item>, Error>
    where
        I: IntoIterator,
        I::Item: Haystack,
    {
        let (send, recv) = oneshot::channel();
        let mut window = SweepWindow::new_from_options(
            options.unwrap_or_else(|| self.options.clone()),
            uid.clone(),
            items_context,
            self.term_waker.clone(),
            None,
            Arc::new({
                let send = std::sync::Mutex::new(Some(send)); // one shot is moved on send
                let uid = uid.clone();
                move |event| {
                    if let SweepEvent::Select { items, .. } = event {
                        if let Some(send) = send.with_mut(|send| send.take()) {
                            _ = send.send(items);
                        }
                        Ok(WindowAction::Close {
                            uid: Some(uid.clone()),
                        })
                    } else {
                        Ok(WindowAction::Nothing)
                    }
                }
            }),
            self.ranker_thread.clone(),
        )?;
        window.haystack_extend(items.into_iter().collect());
        let (send_switch, recv_switch) = oneshot::channel();
        self.send_request(SweepRequest::WindowSwitch {
            window: Either::Left(Box::new(window)),
            close: false,
            created: send_switch,
        });
        if !recv_switch.await? {
            anyhow::bail!("window with this uid already exits: {uid:?}");
        }
        Ok(recv.await.unwrap_or(Vec::new()))
    }

    /// Wait for single event in the asynchronous context
    pub async fn next_event(&self) -> Option<SweepEvent<H>> {
        let mut receiver = self.events.lock().await;
        receiver.recv().await
    }

    /// Wait for sweep to correctly terminate and cleanup terminal
    pub async fn terminate(&self) {
        let _ = self.requests.send(SweepRequest::Terminate);
        let _ = self.term_waker.wake();
        if let Some(terminated) = self.terminated.with_mut(|t| t.take()) {
            let _ = terminated.await;
        }
    }
}

pub struct SweepInner<H: Haystack> {
    options: SweepOptions,
    haystack_context: H::Context,
    term_waker: TerminalWaker,
    ui_worker: Option<JoinHandle<Result<(), Error>>>,
    ranker_thread: RankerThread,
    requests: Sender<SweepRequest<H>>,
    events: Mutex<mpsc::UnboundedReceiver<SweepEvent<H>>>,
    terminated: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl<H: Haystack> SweepInner<H> {
    pub fn new(mut options: SweepOptions, haystack_context: H::Context) -> Result<Self, Error> {
        if options.scorers.is_empty() {
            options.scorers = ALL_SCORER_BUILDERS.clone();
        }
        let (requests_send, requests_recv) = unbounded();
        let (events_send, events_recv) = mpsc::unbounded_channel();
        let (terminate_send, terminate_recv) = oneshot::channel();
        let term = SystemTerminal::open(&options.tty_path)
            .with_context(|| format!("failed to open terminal: {}", options.tty_path))?;
        let term_waker = term.waker();
        let ranker_thread = RankerThread::new({
            let term_waker = term_waker.clone();
            move |_, _| term_waker.wake().is_ok()
        });
        let worker = Builder::new().name("sweep-ui".to_string()).spawn({
            let options = options.clone();
            let haystack_context = haystack_context.clone();
            let ranker_thread = ranker_thread.clone();
            move || {
                sweep_ui_worker(
                    options,
                    term,
                    ranker_thread,
                    requests_recv,
                    events_send,
                    haystack_context,
                )
                .inspect(|_result| {
                    let _ = terminate_send.send(());
                })
            }
        })?;
        Ok(SweepInner {
            options,
            haystack_context,
            term_waker,
            ui_worker: Some(worker),
            ranker_thread,
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
        self.term_waker.wake().unwrap_or(());
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
                    let uid = params.take_opt(0, "uid")?;
                    let items = params.take_seed(VecDeserializeSeed(seed), 1, "items")?;
                    sweep.items_extend(uid, items);
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
                    let uid = params.take_opt(0, "uid")?;
                    let index = params.take(1, "index")?;
                    let item = params.take_seed(seed, 2, "item")?;
                    sweep.item_update(uid, index, item);
                    Ok(Value::Null)
                }
            }
        });

        // items clear
        peer.register("items_clear", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    sweep.items_clear(uid);
                    Ok(Value::Null)
                }
            }
        });

        // items current
        peer.register("items_current", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    let current = sweep
                        .items_current(uid)
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
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    let items = serde_json::to_value(sweep.items_marked(uid).await?)?;
                    Ok(items)
                }
            }
        });

        peer.register("cursor_set", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    let position = params.take(1, "position")?;
                    sweep.cursor_set(uid, position);
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
                    let uid = params.take_opt(0, "uid")?;
                    let query: String = params.take(1, "query")?;
                    sweep.query_set(uid, query);
                    Ok(Value::Null)
                }
            }
        });

        // query get
        peer.register("query_get", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    Ok(sweep.query_get(uid).await?)
                }
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
                    let uid = params.take_opt(0, "uid")?;
                    let prompt: Option<String> = params.take_opt(1, "prompt")?;
                    let icon: Option<Glyph> = params.take_opt(2, "icon")?;
                    sweep.prompt_set(uid, prompt, icon);
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
                    let uid = params.take_opt(0, "uid")?;
                    let theme = sweep.theme_get(uid.clone()).await?;
                    let seed = ViewDeserializer::new(Some(&theme.named_colors), view_cache);
                    let footer: Option<Arc<dyn View>> = params.take_opt_seed(&seed, 1, "footer")?;
                    sweep.footer_set(uid, footer);
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
                    let uid = params.take_opt(0, "uid")?;
                    let chord: KeyChord = params.take(1, "key")?;
                    let tag: String = params.take(2, "tag")?;
                    let desc: Option<String> = params.take_opt(3, "desc")?;
                    sweep.bind(uid, chord, tag, desc.unwrap_or_default());
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
                    let uid = params.take_opt(0, "uid")?;
                    let value: Option<bool> = params.take_opt(1, "value")?;
                    sweep.preview_set(uid, value);
                    Ok(Value::Null)
                }
            }
        });

        // window stack push
        peer.register("window_switch", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take(0, "uid")?;
                    let close = params.take_opt(0, "close")?.unwrap_or(false);
                    Ok(sweep.window_switch(uid, close).await?)
                }
            }
        });

        // window stack push
        peer.register("window_pop", {
            let sweep = self.clone();
            move |_params: Value| {
                sweep.window_pop();
                future::ok(Value::Null)
            }
        });

        // show quick select
        peer.register("quick_select", {
            let sweep = self.clone();
            let seed = seed.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                let seed = seed.clone();
                async move {
                    let items = params.take_seed(VecDeserializeSeed(seed), 0, "items")?;
                    let mut options = sweep.options.clone();
                    if let Some(prompt) = params.take_opt(1, "prompt")? {
                        options.prompt = prompt;
                    }
                    if let Some(prompt_icon) = params.take_opt(2, "prompt_icon")? {
                        options.prompt_icon = prompt_icon;
                    }
                    if let Some(keep_order) = params.take_opt(3, "keep_order")? {
                        options.keep_order = keep_order;
                    }
                    if let Some(theme) = params.take_opt::<String>(4, "theme")? {
                        options.theme = theme.parse()?;
                    }
                    if let Some(scorer) = params.take_opt::<Cow<'_, str>>(5, "scorer")? {
                        scorer_by_name(&mut options.scorers, Some(scorer.as_ref()));
                    }
                    let uid = params.take(6, "uid")?;
                    let result = sweep
                        .quick_select(Some(options), uid, sweep.haystack_context.clone(), items)
                        .await?;
                    let result_value = serde_json::to_value(result)?;
                    Ok(result_value)
                }
            }
        });

        // render_suppress
        peer.register("render_suppress", {
            let sweep = self.clone();
            move |mut params: RpcParams| {
                let sweep = sweep.clone();
                async move {
                    let uid = params.take_opt(0, "uid")?;
                    let suppress = params.take(1, "suppress")?;
                    sweep.render_suppress(uid, suppress);
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
                        SweepEvent::Bind { uid, tag, chord } => peer.notify_with_value(
                            "bind",
                            json!({"uid":  uid, "tag": tag, "key": chord}),
                        )?,
                        SweepEvent::Select { uid, items } => {
                            if !items.is_empty() {
                                peer.notify_with_value(
                                    "select",
                                    json!({"uid": uid, "items": items}),
                                )?
                            }
                        }
                        SweepEvent::Window(window_event) => {
                            let (method, args) = match window_event {
                                WindowEvent::Closed(window_id) => {
                                    ("window_closed", json!({"to": window_id}))
                                }
                                WindowEvent::Opened(window_id) => {
                                    ("window_opened", json!({"to": window_id}))
                                }
                                WindowEvent::Switched { from, to } => {
                                    ("window_switched", json!({"from": from, "to": to}))
                                }
                            };
                            peer.notify_with_value(method, args)?
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
                result = serve => {
                    tracing::info!(?result, "[serve_seed] serve terminated");
                    result
                },
                result = events => {
                    tracing::info!(?result, "[serve_seed] events closed");
                    result
                },
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
        tag: Arc<str>,
        desc: String,
    },
    Select,
    SelectByIndex(usize),
    Mark,
    MarkAll,
    Quit,
    Help,
    ScorerNext,
    PreviewToggle,
    PreviewLineNext,
    PreviewPageNext,
    PreviewLinePrev,
    PreviewPagePrev,
    Input(InputAction),
    List(ListAction),
}

impl SweepAction {
    fn description(&self) -> ActionDesc {
        use SweepAction::*;
        match self {
            User { chord, tag, desc } => ActionDesc {
                chords: vec![chord.clone()],
                name: tag.to_string(),
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
            SelectByIndex(index) => ActionDesc {
                chords: Vec::new(),
                name: format!("sweep.select.{}", index),
                description: format!("Select item by index {}", index),
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
            PreviewPageNext => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('j'),
                    mode: KeyMod::ALT | KeyMod::SHIFT,
                }])],
                name: "sweep.preview.page.next".to_owned(),
                description: "Scroll preview one page down".to_owned(),
            },
            PreviewLinePrev => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::ALT,
                }])],
                name: "sweep.preview.line.prev".to_owned(),
                description: "Scroll preview one line up".to_owned(),
            },
            PreviewPagePrev => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::ALT | KeyMod::SHIFT,
                }])],
                name: "sweep.preview.page.prev".to_owned(),
                description: "Scroll preview one page up".to_owned(),
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
            PreviewPageNext,
            PreviewLinePrev,
            PreviewPagePrev,
        ]
        .into_iter()
        .chain(InputAction::all().map(Input))
        .chain(ListAction::all().map(List))
    }
}

type SweepEventHandler<H> = Arc<dyn Fn(SweepEvent<H>) -> Result<WindowAction, Error> + Send + Sync>;

/// Object representing current state of the sweep worker
struct SweepWindow<H: Haystack> {
    // window unique id
    window_uid: WindowId,
    // waker
    term_waker: TerminalWaker,
    // request queue
    requests: Option<Receiver<SweepWindowRequest<H>>>,
    // event handler
    event_handler: SweepEventHandler<H>,
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
    key_empty_backspace: Option<Arc<str>>,
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
    // this sweep state is used to render help
    is_help: bool,
    // marked items (multi-select)
    marked: Arc<RwLock<MarkedItems<H>>>,
    // ranker
    ranker: Ranker,
    // haystack
    haystack: Vec<H>,
    // haystack keymap
    haystack_keymap: KeyMapHandler<SweepAction>,
    // haystack context
    haystack_context: H::Context,
    // cached large preview of the current item
    preview_large: Option<SweepPreview<H::PreviewLarge>>,
    // None - rendering is not suppressed
    // Some(false) - resumed but not synchronized
    // Some(true) - resumed and should be converted to None
    render_suppress_sync: Option<Arc<AtomicBool>>,
}

impl<H> SweepWindow<H>
where
    H: Haystack,
{
    fn new_from_options(
        options: SweepOptions,
        window_uid: WindowId,
        haystack_context: H::Context,
        term_waker: TerminalWaker,
        requests: Option<Receiver<SweepWindowRequest<H>>>,
        event_handler: SweepEventHandler<H>,
        ranker_thread: RankerThread,
    ) -> Result<Self, Error> {
        let ranker = Ranker::new(ranker_thread)?;
        ranker.scorer_set(options.scorers[0].clone());
        ranker.keep_order(Some(options.keep_order));
        Ok(SweepWindow::new(
            window_uid,
            options.prompt,
            options.prompt_icon,
            ranker,
            options.theme,
            options.scorers,
            haystack_context,
            term_waker,
            requests,
            event_handler,
            false,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        window_uid: WindowId,
        prompt: String,
        prompt_icon: Option<Glyph>,
        ranker: Ranker,
        theme: Theme,
        scorers: VecDeque<ScorerBuilder>,
        haystack_context: H::Context,
        term_waker: TerminalWaker,
        requests: Option<Receiver<SweepWindowRequest<H>>>,
        event_handler: SweepEventHandler<H>,
        is_help: bool,
    ) -> Self {
        let mut key_map = KeyMap::new();
        let mut key_actions = HashMap::new();
        for action in SweepAction::all() {
            let desc = action.description();
            key_actions.insert(desc.name, action.clone());
            for chord in desc.chords {
                key_map.register(chord, action.clone());
            }
        }
        Self {
            window_uid,
            term_waker,
            requests,
            event_handler,
            scorers,
            prompt,
            prompt_icon,
            footer: None,
            key_map_state: Vec::new(),
            key_empty_backspace: None,
            key_map,
            key_actions,
            theme: theme.clone(),
            input: Input::new(theme.clone()),
            list: List::new(
                SweepItems::new(Arc::new(RankedItems::default()), Default::default()),
                theme,
            ),
            marked: Default::default(),
            ranker,
            haystack: Vec::new(),
            haystack_keymap: KeyMapHandler::new(),
            haystack_context,
            preview_large: None,
            render_suppress_sync: None,
            is_help,
        }
    }

    fn haystack_extend(&mut self, haystack: Vec<H>) {
        self.ranker
            .haystack_extend(&self.haystack_context, &haystack);
        let index_offset = self.haystack.len();
        haystack.iter().enumerate().for_each(|(index, haystack)| {
            let Some(chord) = haystack.hotkey() else {
                return;
            };
            self.haystack_keymap.register(
                chord.as_ref(),
                SweepAction::SelectByIndex(index_offset + index),
            );
        });
        self.haystack.extend(haystack);
    }

    // get currently pointed item
    fn current(&self) -> Option<SweepItem<'_, H>> {
        let sweep_items = self.list.items();
        self.list.current().and_then(|id| {
            let score = sweep_items.ranked_items.get(id.rank_index)?;
            let haystack = self.haystack.get(id.haystack_index)?;
            Some(SweepItem {
                id,
                score,
                haystack,
            })
        })
    }

    // get preview of the currently pointed haystack item
    fn current_preview(&self) -> Option<H::Preview> {
        let item = self.current()?;
        item.haystack
            .preview(&self.haystack_context, item.score.positions, &self.theme)
    }

    // get large preview for currently pointed haystack item
    fn current_preview_large(&mut self) -> Option<SweepPreview<H::PreviewLarge>> {
        let item = self.current()?;
        if !matches!(&self.preview_large, Some(preview) if preview.id == item.id) {
            let preview = item.haystack.preview_large(
                &self.haystack_context,
                item.score.positions,
                &self.theme,
            )?;
            self.preview_large = Some(SweepPreview::new(item.id, self.theme.clone(), preview));
        }
        self.preview_large.clone()
    }

    // update theme
    fn theme_set(&mut self, theme: Theme) {
        self.input.theme_set(theme.clone());
        self.list.theme_set(theme.clone());
        self.theme = theme;
    }

    /// Trigger ranker, should be called whenever needle might have changed
    fn ranker_trigger(&self) {
        // ranker only runs if needle has actually been updated, so it is safe
        // to run whenever needle might have changed
        self.ranker.needle_set(self.input.get().collect());
    }

    /// Retrieve latest ranker result and update list view
    fn ranker_refresh(&mut self) -> Arc<RankedItems> {
        // check if list view needs to be updated
        let ranker_result = self.ranker.result();
        if self.list.items().generation() != ranker_result.generation() {
            // find cursor position of currently pointed item in the new result
            let cursor = if self.list.cursor() == 0 {
                None
            } else {
                self.current()
                    .and_then(|item| ranker_result.find_match_index(item.id.haystack_index))
            };
            // update list with new results
            let _old_items = self
                .list
                .items_set(SweepItems::new(ranker_result.clone(), self.marked.clone()));
            if let Some(cursor) = cursor {
                self.list.cursor_set(cursor);
            }
            // dropping old result might add noticeable delay for large lists
            // rayon::spawn(move || std::mem::drop(old_items));
        }
        ranker_result
    }

    fn handle_action(&mut self, action: &SweepAction) -> Result<WindowAction, Error> {
        match action {
            SweepAction::Input(action) => {
                self.input.apply(action);
                self.ranker_trigger();
            }
            SweepAction::List(action) => self.list.apply(action),
            SweepAction::User { tag, chord, .. } => {
                if !tag.is_empty() {
                    return (self.event_handler)(SweepEvent::Bind {
                        uid: self.window_uid.clone(),
                        tag: tag.clone(),
                        chord: chord.clone(),
                    });
                }
            }
            SweepAction::Quit => return Ok(WindowAction::Close { uid: None }),
            SweepAction::Select => {
                let selected: Vec<H> = if self.marked.with(|marked| !marked.is_empty()) {
                    self.marked.with_mut(|marked| marked.take()).collect()
                } else {
                    self.current()
                        .map(|item| item.haystack.clone())
                        .into_iter()
                        .collect()
                };
                return (self.event_handler)(SweepEvent::Select {
                    uid: self.window_uid.clone(),
                    items: selected,
                });
            }
            SweepAction::SelectByIndex(index) => {
                if let Some(item) = self.haystack.get(*index) {
                    return (self.event_handler)(SweepEvent::Select {
                        uid: self.window_uid.clone(),
                        items: vec![item.clone()],
                    });
                }
            }
            SweepAction::Mark => {
                if let Some(item) = self.current() {
                    self.marked
                        .with_mut(|marked| marked.toggle(item.id, item.haystack.clone()));
                    self.list.apply(&ListAction::ItemNext);
                }
            }
            SweepAction::MarkAll => {
                self.marked.with_mut(|marked| {
                    if marked.is_empty() {
                        // mark all
                        let ranked_items = &self.list.items().ranked_items;
                        let (haystack_gen, rank_gen) = ranked_items.generation();
                        for score in ranked_items.iter() {
                            let Some(haystack) = self.haystack.get(score.haystack_index) else {
                                continue;
                            };
                            marked.toggle(
                                SweepItemId {
                                    haystack_gen,
                                    haystack_index: score.haystack_index,
                                    rank_gen,
                                    rank_index: score.rank_index,
                                },
                                haystack.clone(),
                            )
                        }
                    } else {
                        // un-mark all
                        _ = marked.take();
                    }
                })
            }
            SweepAction::Help => {
                if !self.is_help {
                    return Ok(WindowAction::Open {
                        window: self.help()?,
                        close: false,
                    });
                }
            }
            SweepAction::ScorerNext => {
                if let Some(scorer) = scorer_by_name(&mut self.scorers, None) {
                    self.ranker.scorer_set(scorer);
                }
            }
            SweepAction::PreviewToggle => self.theme_set(
                self.theme
                    .modify(|inner| inner.show_preview = !self.theme.show_preview),
            ),
            SweepAction::PreviewLineNext | SweepAction::PreviewPageNext => {
                if let Some(preview) = self.preview_large.as_ref() {
                    let delta = if matches!(action, SweepAction::PreviewPageNext) {
                        preview.height.load(Ordering::Relaxed)
                    } else {
                        1
                    };
                    let layout = preview.preview.preview_layout();
                    let mut offset = layout.position();
                    offset.row = layout.size().height.min(offset.row + delta);
                    preview.preview.set_offset(offset);
                }
            }
            SweepAction::PreviewLinePrev | SweepAction::PreviewPagePrev => {
                if let Some(preview) = self.preview_large.as_ref() {
                    let delta = if matches!(action, SweepAction::PreviewPagePrev) {
                        preview.height.load(Ordering::Relaxed)
                    } else {
                        1
                    };
                    let layout = preview.preview.preview_layout();
                    let mut offset = layout.position();
                    offset.row = offset.row.saturating_sub(delta);
                    preview.preview.set_offset(offset);
                }
            }
        }
        Ok(WindowAction::Nothing)
    }

    /// Crate sweep states which renders help view
    fn help(&self) -> Result<Box<dyn Window>, Error> {
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

        let help_uid = self.uid().with_suffix("help");
        let parent_uid = self.uid().clone();
        let mut window = SweepWindow::new(
            help_uid.clone(),
            "HELP".to_owned(),
            Some(KEYBOARD_ICON.clone()),
            Ranker::new(self.ranker.ranker_thread().clone())?,
            self.theme.modify(|inner| inner.show_preview = true),
            self.scorers.clone(),
            (),
            self.term_waker.clone(),
            None,
            Arc::new(move |event| {
                if let SweepEvent::Select {
                    items: selected, ..
                } = event
                {
                    let name = selected
                        .into_iter()
                        .next()
                        .map(|action: ActionDesc| action.name);
                    Ok(WindowAction::Switch {
                        uid: parent_uid.clone(),
                        args: name.into(),
                        close: true,
                    })
                } else {
                    Ok(WindowAction::Nothing)
                }
            }),
            true,
        );
        window.ranker.keep_order(Some(true));
        window.haystack_extend(entries);
        Ok(Box::new(window))
    }
}

impl<H: Haystack> Window for SweepWindow<H> {
    fn uid(&self) -> &WindowId {
        &self.window_uid
    }

    fn process(&mut self) -> Result<WindowAction, Error> {
        let Some(requests) = self.requests.clone() else {
            return Ok(WindowAction::Nothing);
        };
        for request in requests.try_iter() {
            use SweepWindowRequest::*;
            match request {
                NeedleSet(needle) => {
                    self.input.set(needle.as_ref());
                    self.ranker_trigger();
                }
                NeedleGet(resolve) => {
                    mem::drop(resolve.send(self.input.get().collect()));
                }
                ThemeGet(resolve) => {
                    mem::drop(resolve.send(self.theme.clone()));
                }
                Bind { chord, tag, desc } => match *chord.keys() {
                    [
                        Key {
                            name: KeyName::Backspace,
                            mode: KeyMod::EMPTY,
                        },
                    ] => {
                        self.key_empty_backspace = if tag.is_empty() {
                            None
                        } else {
                            Some(tag.into())
                        };
                    }
                    _ => {
                        let action = if tag.is_empty() {
                            // empty user action means unbind
                            SweepAction::User {
                                chord: KeyChord::new(Vec::new()),
                                tag: Default::default(),
                                desc: String::new(),
                            }
                        } else {
                            self.key_actions
                                .entry(tag.clone())
                                .or_insert_with(|| SweepAction::User {
                                    chord: chord.clone(),
                                    tag: tag.into(),
                                    desc,
                                })
                                .clone()
                        };
                        self.key_map.register(chord.as_ref(), action);
                    }
                },
                PromptSet(new_prompt, new_icon) => {
                    if let Some(new_prompt) = new_prompt {
                        self.prompt = new_prompt;
                    }
                    self.prompt_icon = new_icon;
                }
                Current(resolve) => {
                    _ = resolve.send(self.current().map(|item| item.haystack.clone()));
                }
                Marked(resolve) => {
                    let items = self.marked.with_mut(|marked| marked.take()).collect();
                    _ = resolve.send(items);
                }
                CursorSet { position } => {
                    self.list.cursor_set(position);
                }
                ScorerByName(name, resolve) => {
                    let _ = match scorer_by_name(&mut self.scorers, name.as_deref()) {
                        None => resolve.send(false),
                        Some(scorer) => {
                            self.ranker.scorer_set(scorer);
                            resolve.send(true)
                        }
                    };
                }
                PreviewSet(value) => {
                    let show_preview = match value {
                        Some(value) => value,
                        None => !self.theme.show_preview,
                    };
                    self.theme_set(self.theme.modify(|inner| inner.show_preview = show_preview));
                }
                FooterSet(view) => self.footer = view,
                ScorerSet(scorer) => self.ranker.scorer_set(scorer),
                HaystackExtend(items) => {
                    self.haystack_extend(items);
                }
                HaystackUpdate { index, item } => {
                    if let Some(item_ref) = self.haystack.get_mut(index) {
                        *item_ref = item;
                    }
                }
                HaystackClear => {
                    self.ranker.haystack_clear();
                    self.haystack.clear();
                    self.haystack_keymap.clear();
                }
                RankerKeepOrder(toggle) => self.ranker.keep_order(toggle),
                RenderSuppress(suppress) => {
                    self.render_suppress_sync = if suppress {
                        Some(Arc::new(AtomicBool::new(false)))
                    } else {
                        Some(self.ranker.sync())
                    };
                }
            }
        }
        Ok(WindowAction::Nothing)
    }

    fn resume(&mut self, args: Value) -> Result<WindowAction, Error> {
        match args {
            Value::String(action_name) => {
                // handle action picked by help window
                if let Some(action) = self.key_actions.get(&action_name) {
                    self.handle_action(&action.clone())
                } else {
                    Ok(WindowAction::Nothing)
                }
            }
            _ => Ok(WindowAction::Nothing),
        }
    }

    fn handle_key(&mut self, key: Key) -> Result<WindowAction, Error> {
        let is_first_key = self.key_map_state.is_empty();
        if let Some(action) = self.key_map.lookup_state(&mut self.key_map_state, key) {
            tracing::debug!(?action, "[SweepState.handle_key]");
            // do not generate Backspace, when input is not empty
            let backspace = Key::new(KeyName::Backspace, KeyMod::EMPTY);
            if is_first_key && key == backspace && self.input.get().count() == 0 {
                if let Some(ref tag) = self.key_empty_backspace {
                    return (self.event_handler)(SweepEvent::Bind {
                        uid: self.window_uid.clone(),
                        tag: tag.clone(),
                        chord: KeyChord::from_iter([backspace]),
                    });
                }
            } else {
                return self.handle_action(&action.clone());
            }
        } else if let Some(action) = self.haystack_keymap.handle(key).cloned() {
            return self.handle_action(&action);
        } else if let Key {
            name: KeyName::Char(c),
            mode: KeyMod::EMPTY,
        } = key
        {
            // send plain chars to the input
            self.input.apply(&InputAction::Insert(c));
            self.ranker_trigger();
        }
        Ok(WindowAction::Nothing)
    }

    fn handle_mouse(&mut self, mouse: Mouse, tag: &Value) -> Result<WindowAction, Error> {
        let Value::String(tag) = &tag else {
            return Ok(WindowAction::Nothing);
        };
        match self.key_actions.get(tag) {
            Some(action) if mouse.mode == KeyMod::EMPTY => {
                // trigger state bound actions on release
                self.handle_action(&action.clone())
            }
            _ if mouse.mode.contains(KeyMod::PRESS) => {
                // ignore press events, and trigger on release
                Ok(WindowAction::Nothing)
            }
            _ => {
                let key = Key::new(mouse.name, mouse.mode);
                (self.event_handler)(SweepEvent::Bind {
                    uid: self.window_uid.clone(),
                    tag: tag.as_str().into(),
                    chord: KeyChord::from_iter([key]),
                })
            }
        }
    }

    fn view(&mut self, term_position: Position, sweep_layout: WindowLayout) -> Option<BoxView<'_>> {
        if !self
            .render_suppress_sync
            .as_ref()
            .map_or_else(|| true, |s| s.load(Ordering::Acquire))
        {
            return None;
        }
        self.render_suppress_sync.take();

        self.ranker_refresh();
        let large_preview = self.current_preview_large().map(|v| v.into_view());
        let sweep_view = self.into_view();
        let view = SweepLayoutView {
            sweep_view,
            large_preview,
            sweep_layout,
            term_position,
        };
        Some(view.boxed())
    }
}

impl<'a, H: Haystack> IntoView for &'a mut SweepWindow<H> {
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
                &format_args!("{}/{} ", ranker_result.len(), self.haystack.len(),),
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

        let header = FlexRef::row((
            prompt.into(),
            FlexChild::new(&self.input).flex(1.0),
            View::tag(stats, Value::String(SWEEP_SCORER_NEXT_TAG.to_string())).into(),
        ));

        let body = FlexRef::row((
            // list
            FlexChild::new(self.list.view(SweepItemsContext {
                haystack_context: self.haystack_context.clone(),
                haystack: &self.haystack,
            }))
            .flex(1.0),
            // preview
            self.theme
                .show_preview
                .then(|| self.current_preview())
                .flatten()
                .map_or_else(
                    || FlexChild::new(().left_view()).flex(0.0),
                    |preview| {
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
                        FlexChild::new(view.right_view()).flex(flex)
                    },
                ),
            // scrollbar
            self.list.scroll_bar().into(),
        ));

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

#[derive(Clone, Default)]
struct SweepWindowDispatch<H> {
    channels: Arc<RwLock<HashMap<WindowId, Sender<SweepWindowRequest<H>>>>>,
}

impl<H> SweepWindowDispatch<H> {
    fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::default())),
        }
    }

    fn handle(&self, uid: &WindowId, request: SweepWindowRequest<H>) -> bool {
        let Some(send) = self.channels.with(|channels| channels.get(uid).cloned()) else {
            return false;
        };
        if send.send(request).is_err() {
            self.channels.with_mut(|channels| channels.remove(uid));
            false
        } else {
            true
        }
    }

    fn create(&self, uid: WindowId) -> Result<Receiver<SweepWindowRequest<H>>, Error> {
        self.channels.with_mut(|channels| {
            let (send, recv) = unbounded();
            if channels.contains_key(&uid) {
                anyhow::bail!("uid already exits: {uid:?}")
            }
            channels.insert(uid, send);
            Ok(recv)
        })
    }

    fn remove(&self, uid: &WindowId) -> Option<Sender<SweepWindowRequest<H>>> {
        self.channels.with_mut(|channels| channels.remove(uid))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(untagged)]
pub enum WindowId {
    String(Arc<str>),
    Number(u64),
}

impl WindowId {
    fn with_suffix(&self, suffix: &str) -> Self {
        match self {
            WindowId::String(name) => WindowId::String(format!("{name}/{suffix}").into()),
            WindowId::Number(name) => WindowId::String(format!("{name}/{suffix}").into()),
        }
    }
}

impl From<String> for WindowId {
    fn from(value: String) -> Self {
        WindowId::String(value.into())
    }
}

impl From<&str> for WindowId {
    fn from(value: &str) -> Self {
        WindowId::String(value.into())
    }
}

impl From<u64> for WindowId {
    fn from(value: u64) -> Self {
        WindowId::Number(value)
    }
}

#[derive(Debug, Clone)]
pub enum WindowEvent {
    Closed(WindowId),
    Opened(WindowId),
    Switched {
        from: Option<WindowId>,
        to: WindowId,
    },
}

/// Action generated by the window
enum WindowAction {
    /// Quit sweep
    Quit,
    /// Close window (active window if uid is not specified)
    Close {
        uid: Option<WindowId>,
    },
    /// Open new window
    Open {
        window: Box<dyn Window>,
        close: bool,
    },
    /// To provided uid passing arguments to resume, close current window if close is set
    Switch {
        uid: WindowId,
        args: Value,
        close: bool,
    },
    Nothing,
}

trait Window: Send + Sync {
    /// Window unique identifier
    fn uid(&self) -> &WindowId;

    /// Process pending on every loop
    fn process(&mut self) -> Result<WindowAction, Error>;

    /// Resume window with provided arguments
    fn resume(&mut self, args: Value) -> Result<WindowAction, Error>;

    /// Handle keyboard event
    fn handle_key(&mut self, key: Key) -> Result<WindowAction, Error>;

    /// Handle mouse event
    fn handle_mouse(&mut self, mouse: Mouse, tag: &Value) -> Result<WindowAction, Error>;

    /// Window view, `None` means do not update
    fn view(&mut self, term_position: Position, sweep_layout: WindowLayout) -> Option<BoxView<'_>>;
}

impl<M: Window + ?Sized> Window for Box<M> {
    fn uid(&self) -> &WindowId {
        (**self).uid()
    }

    fn process(&mut self) -> Result<WindowAction, Error> {
        (**self).process()
    }

    fn resume(&mut self, args: Value) -> Result<WindowAction, Error> {
        (**self).resume(args)
    }

    fn handle_key(&mut self, key: Key) -> Result<WindowAction, Error> {
        (**self).handle_key(key)
    }

    fn handle_mouse(&mut self, mouse: Mouse, tag: &Value) -> Result<WindowAction, Error> {
        (**self).handle_mouse(mouse, tag)
    }

    fn view(&mut self, term_position: Position, sweep_layout: WindowLayout) -> Option<BoxView<'_>> {
        (**self).view(term_position, sweep_layout)
    }
}

struct WindowStack<T> {
    windows: Vec<Box<dyn Window>>,
    transition_handler: T,
}

impl<T> WindowStack<T>
where
    T: FnMut(WindowEvent) -> Result<(), Error>,
{
    fn new(transition_handler: T) -> Self {
        Self {
            windows: Default::default(),
            transition_handler,
        }
    }

    /// Find window position by its uid
    fn window_position(&self, uid: &WindowId) -> Option<usize> {
        self.windows.iter().position(|window| window.uid() == uid)
    }

    /// Currently active window
    fn window_current(&mut self) -> Option<&mut Box<dyn Window>> {
        self.windows.last_mut()
    }

    fn window_close(&mut self, uid: Option<WindowId>) -> Result<WindowAction, Error> {
        let Some(uid) = uid.or_else(|| self.window_current().map(|win| win.uid().clone())) else {
            return Ok(WindowAction::Quit);
        };
        let Some(index) = self.window_position(&uid) else {
            return Ok(WindowAction::Nothing);
        };
        let window_from = self.windows.remove(index);
        (self.transition_handler)(WindowEvent::Closed(window_from.uid().clone()))?;

        // closed window was active
        if index == self.windows.len() {
            if let Some(window_to) = self.window_current() {
                let uid_to = window_to.uid().clone();
                let action = window_to.resume(Value::Null)?;
                (self.transition_handler)(WindowEvent::Switched {
                    from: Some(window_from.uid().clone()),
                    to: uid_to,
                })?;
                return Ok(action);
            }
        }

        Ok(WindowAction::Nothing)
    }

    fn window_open(
        &mut self,
        window_to: Box<dyn Window>,
        close: bool,
    ) -> Result<WindowAction, Error> {
        let uid_to = window_to.uid().clone();
        // existing window
        if self.window_position(&uid_to).is_some() {
            return Ok(WindowAction::Switch {
                uid: window_to.uid().clone(),
                args: Value::Null,
                close: false,
            });
        }

        let uid_from = if close {
            if let Some(window) = self.windows.pop() {
                let uid = window.uid().clone();
                (self.transition_handler)(WindowEvent::Closed(uid.clone()))?;
                Some(uid)
            } else {
                None
            }
        } else {
            self.window_current().map(|win| win.uid().clone())
        };

        // new
        self.windows.push(window_to);
        (self.transition_handler)(WindowEvent::Opened(uid_to.clone()))?;

        // resume
        if let Some(window) = self.window_current() {
            let action = window.resume(Value::Null)?;
            (self.transition_handler)(WindowEvent::Switched {
                from: uid_from,
                to: uid_to,
            })?;
            return Ok(action);
        }

        Ok(WindowAction::Nothing)
    }

    fn window_switch(
        &mut self,
        uid_to: WindowId,
        args: Value,
        close: bool,
    ) -> Result<WindowAction, Error> {
        let Some(index_to) = self.window_position(&uid_to) else {
            // no such window
            return Ok(WindowAction::Nothing);
        };
        let index_from = self.windows.len() - 1;
        if index_from == index_to {
            // window already active
            return Ok(WindowAction::Nothing);
        }
        let uid_from = self.windows[index_from].uid().clone();

        if close {
            let Some(window) = self.windows.pop() else {
                return Ok(WindowAction::Nothing);
            };
            (self.transition_handler)(WindowEvent::Closed(window.uid().clone()))?;
        }

        // swap with last
        let last_index = self.windows.len() - 1;
        self.windows.swap(index_to, last_index);

        // resume
        if let Some(window) = self.window_current() {
            let action = window.resume(args)?;
            (self.transition_handler)(WindowEvent::Switched {
                from: Some(uid_from),
                to: uid_to,
            })?;
            return Ok(action);
        }

        Ok(WindowAction::Nothing)
    }

    // Bool indicates if we want to continue running
    fn handle_action(&mut self, mut action: WindowAction) -> Result<bool, Error> {
        loop {
            action = match action {
                WindowAction::Quit => return Ok(false),
                WindowAction::Nothing => return Ok(true),
                WindowAction::Close { uid } => self.window_close(uid)?,
                WindowAction::Open {
                    window: window_to,
                    close,
                } => self.window_open(window_to, close)?,
                WindowAction::Switch { uid, args, close } => {
                    self.window_switch(uid, args, close)?
                }
            };
        }
    }
}

fn sweep_ui_worker<H>(
    mut options: SweepOptions,
    mut term: SystemTerminal,
    ranker_thread: RankerThread,
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
    let mut term_position = term.position()?;
    let term_scroll = options.layout.scroll(term_position, term_size.cells);
    if term_scroll > 0 {
        term_position.row -= term_scroll;
        term.execute(TerminalCommand::Scroll(term_scroll as i32))?;
    }
    if options.layout.is_altscreen() {
        term.execute(TerminalCommand::altscreen_set(true))?;
    }
    // report size
    events.send(SweepEvent::Resize(term_size))?;

    let mut window_events = Vec::new();
    let mut window_created = false;
    let window_dispatch = SweepWindowDispatch::new();
    let mut window_stack = WindowStack::new({
        let events = events.clone();
        let window_dispatch = window_dispatch.clone();
        move |event| {
            if let WindowEvent::Closed(uid) = &event {
                window_dispatch.remove(uid);
            }
            Ok(events.send(SweepEvent::Window(event))?)
        }
    });

    let event_handler_default = Arc::new({
        let events = events.clone();
        move |event| {
            if events.send(event).is_err() {
                anyhow::bail!("sweep events queue is closed");
            }
            Ok(WindowAction::Nothing)
        }
    });
    if let Some(window_uid) = options.window_uid.clone() {
        window_created = true;
        let window = Box::new(SweepWindow::new_from_options(
            options.clone(),
            window_uid.clone(),
            haystack_context.clone(),
            term.waker(),
            Some(window_dispatch.create(window_uid)?),
            event_handler_default.clone(),
            ranker_thread.clone(),
        )?);
        if !window_stack.handle_action(WindowAction::Open {
            window,
            close: false,
        })? {
            return Ok(());
        };
    }

    let mut layout_store = ViewLayoutStore::new();
    let mut layout_id: Option<TreeId> = None;

    // render loop
    term.waker().wake()?; // schedule one wake just in case if it was consumed by previous poll
    let result = term.run_render(|term, event, mut surf| {
        // process requests
        for request in requests.try_iter() {
            use SweepRequest::*;
            let window_event = match request {
                Terminate => return Ok(TerminalAction::Quit(())),
                WindowSwitch {
                    window,
                    created,
                    close,
                } => {
                    let uid = window.as_ref().either(|win| win.uid(), |uid| uid);
                    if window_stack.window_position(uid).is_some() {
                        _ = created.send(false);
                        WindowAction::Switch {
                            uid: uid.clone(),
                            args: Value::Null,
                            close,
                        }
                    } else {
                        window_created = true;
                        _ = created.send(true);
                        let window = window.either(Ok::<_, Error>, |uid| {
                            let win = Box::new(SweepWindow::new_from_options(
                                options.clone(),
                                uid.clone(),
                                haystack_context.clone(),
                                term.waker(),
                                Some(window_dispatch.create(uid)?),
                                event_handler_default.clone(),
                                ranker_thread.clone(),
                            )?);
                            Ok(win)
                        })?;
                        WindowAction::Open { window, close }
                    }
                }
                WindowPop => WindowAction::Close { uid: None },
                WindowRequest { uid, request } => {
                    let Some(uid) = uid
                        .clone()
                        .or_else(|| Some(window_stack.window_current()?.uid().clone()))
                    else {
                        tracing::error!(?uid, "[sweep_ui_worker] window not found");
                        return Ok(TerminalAction::Quit(()));
                    };
                    window_dispatch.handle(&uid, request);
                    WindowAction::Nothing
                }
            };
            if !window_stack.handle_action(window_event)? {
                return Ok(TerminalAction::Quit(()));
            }
        }

        // process window state (pending sweep requests)
        for window in window_stack.windows.iter_mut() {
            window_events.push(window.process()?);
        }
        for window_event in window_events.drain(..) {
            if !window_stack.handle_action(window_event)? {
                return Ok(TerminalAction::Quit(()));
            }
        }

        // handle events
        match event {
            Some(TerminalEvent::Resize(term_size)) => {
                term.execute(TerminalCommand::Face(Default::default()))?;
                term.execute(TerminalCommand::EraseScreen)?;
                events.send(SweepEvent::Resize(term_size))?;
            }
            Some(TerminalEvent::Key(key)) => {
                // process window key
                if let Some(window) = window_stack.window_current() {
                    let window_event = window.handle_key(key)?;
                    if !window_stack.handle_action(window_event)? {
                        return Ok(TerminalAction::Quit(()));
                    }
                }
            }
            Some(TerminalEvent::Mouse(mouse)) => {
                let layout = layout_id.map(|layout_id| TreeView::from_id(&layout_store, layout_id));
                let tag = if let Some(layout) = layout.as_ref() {
                    let mut tag: Option<&Value> = None;
                    for child_layout in layout.find_path(mouse.pos) {
                        if let Some(tag_next) = child_layout.data::<Value>() {
                            tag = Some(tag_next);
                        };
                    }
                    tag.unwrap_or(&Value::Null)
                } else {
                    &Value::Null
                };
                // process window mouse key
                if let Some(window) = window_stack.window_current() {
                    let window_event = window.handle_mouse(mouse, tag)?;
                    if !window_stack.handle_action(window_event)? {
                        return Ok(TerminalAction::Quit(()));
                    }
                }
            }
            _ => (),
        }

        // render
        let Some(window) = window_stack.window_current() else {
            let action = if window_created {
                TerminalAction::Quit(())
            } else {
                TerminalAction::WaitNoFrame
            };
            return Ok(action);
        };
        let Some(view) = window.view(term_position, options.layout.clone()) else {
            return Ok(TerminalAction::WaitNoFrame);
        };
        let ctx = ViewContext::new(term)?;
        layout_id = tracing::debug_span!("[sweep_ui_worker][draw]")
            .in_scope(|| Some(surf.draw_view(&ctx, Some(&mut layout_store), view)))
            .transpose()?;

        Ok(TerminalAction::Wait)
    });

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: term_position.row,
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
    ranked_items: Arc<RankedItems>,
    marked_items: Arc<RwLock<MarkedItems<H>>>,
}

impl<H: Haystack> SweepItems<H> {
    fn new(ranked_items: Arc<RankedItems>, marked_items: Arc<RwLock<MarkedItems<H>>>) -> Self {
        Self {
            ranked_items,
            marked_items,
        }
    }

    fn generation(&self) -> (usize, usize) {
        self.ranked_items.generation()
    }
}

struct SweepItemsContext<'a, H: Haystack> {
    haystack_context: H::Context,
    haystack: &'a [H],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SweepItemId {
    pub haystack_gen: usize,
    pub haystack_index: usize,
    pub rank_gen: usize,
    pub rank_index: usize,
}

pub struct SweepItem<'a, H> {
    id: SweepItemId,
    score: ScoreItem<'a>,
    haystack: &'a H,
}

impl<H: Haystack> ListItems for SweepItems<H> {
    type Item = SweepItemId;
    type ItemView = H::View;
    type Context<'a> = SweepItemsContext<'a, H>;

    fn len(&self) -> usize {
        self.ranked_items.len()
    }

    fn get(&self, index: usize) -> Option<Self::Item> {
        let score = self.ranked_items.get(index)?;
        let (haystack_gen, rank_gen) = self.ranked_items.generation();
        Some(SweepItemId {
            haystack_gen,
            haystack_index: score.haystack_index,
            rank_gen,
            rank_index: score.rank_index,
        })
    }

    fn get_view<'a>(
        &'a self,
        item: Self::Item,
        theme: Theme,
        ctx: &'a Self::Context<'a>,
    ) -> Option<Self::ItemView> {
        let score = self.ranked_items.get(item.rank_index)?;
        let haystack = ctx.haystack.get(item.haystack_index)?;
        Some(haystack.view(&ctx.haystack_context, score.positions, &theme))
    }

    fn is_marked(&self, item: &Self::Item) -> bool {
        self.marked_items.with(|marked| marked.contains_id(*item))
    }
}

/// Set of marked (multi-selected) items
struct MarkedItems<H> {
    order_to_haystack: BTreeMap<usize, H>,
    haystack_index_to_order: HashMap<usize, usize>,
    order: usize,
}

impl<H> Default for MarkedItems<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> MarkedItems<H> {
    fn new() -> Self {
        Self {
            order_to_haystack: Default::default(),
            haystack_index_to_order: Default::default(),
            order: 0,
        }
    }

    fn len(&self) -> usize {
        self.haystack_index_to_order.len()
    }

    fn is_empty(&self) -> bool {
        self.haystack_index_to_order.is_empty()
    }

    fn toggle(&mut self, id: SweepItemId, haystack: H) {
        match self.haystack_index_to_order.get(&id.haystack_index) {
            Some(index) => {
                self.order_to_haystack.remove(index);
                self.haystack_index_to_order.remove(&id.haystack_index);
            }
            None => {
                self.haystack_index_to_order
                    .insert(id.haystack_index, self.order);
                self.order_to_haystack.insert(self.order, haystack);
                self.order += 1;
            }
        }
    }

    fn take(&mut self) -> impl Iterator<Item = H> + use<H> {
        self.haystack_index_to_order.clear();
        std::mem::take(&mut self.order_to_haystack).into_values()
    }

    fn contains_id(&self, id: SweepItemId) -> bool {
        self.haystack_index_to_order
            .contains_key(&id.haystack_index)
    }
}

#[derive(Debug, Clone)]
pub enum WindowLayout {
    Float {
        height: WindowLayoutSize,
        width: WindowLayoutSize,
        row: WindowLayoutSize,
        column: WindowLayoutSize,
    },
    Full {
        height: WindowLayoutSize,
    },
}

impl WindowLayout {
    /// Whether alt screen should be enabled
    fn is_altscreen(&self) -> bool {
        matches!(self, WindowLayout::Full { .. })
    }

    /// Whether we need to scroll terminal
    fn scroll(&self, term_position: Position, term_size: Size) -> usize {
        let WindowLayout::Float { row, height, .. } = self else {
            return 0;
        };
        let row = if row.is_full() {
            term_position.row
        } else {
            row.calc(term_size.height)
        };
        let height = height.calc(term_size.height);
        (row + height).saturating_sub(term_size.height)
    }
}

impl Default for WindowLayout {
    fn default() -> Self {
        use WindowLayoutSize::*;
        WindowLayout::Float {
            height: Absolute(11),
            width: Full,
            column: Absolute(0),
            row: Full,
        }
    }
}

impl std::str::FromStr for WindowLayout {
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
                let mut height = WindowLayoutSize::Absolute(11);
                let mut width = WindowLayoutSize::Full;
                let mut column = WindowLayoutSize::Absolute(0);
                let mut row = WindowLayoutSize::Full;
                for (key, value) in kvs {
                    match key {
                        "height" | "h" => height = value.parse()?,
                        "width" | "w" => width = value.parse()?,
                        "column" | "c" => column = value.parse()?,
                        "row" | "r" => row = value.parse()?,
                        _ => {}
                    }
                }
                Ok(WindowLayout::Float {
                    height,
                    width,
                    row,
                    column,
                })
            }
            "full" => {
                let mut height = WindowLayoutSize::Full;
                for (key, value) in kvs {
                    match key {
                        "height" | "h" => height = value.parse()?,
                        _ => {}
                    }
                }
                Ok(WindowLayout::Full { height })
            }
            _ => Err(anyhow::anyhow!("invalid layout name: {}", name)),
        }
    }
}

pub struct SweepLayoutView<V, P> {
    sweep_view: V,
    large_preview: Option<P>,
    sweep_layout: WindowLayout,
    term_position: Position,
}

impl<V, P> View for SweepLayoutView<V, P>
where
    V: View,
    P: View,
{
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        // only structure and order is important for rendering
        match &self.sweep_layout {
            WindowLayout::Float { .. } => {
                let surf = layout.apply_to(surf);
                let child_layout = layout
                    .children()
                    .next()
                    .ok_or(surf_n_term::Error::InvalidLayout)?;
                self.sweep_view.render(ctx, surf, child_layout)
            }
            WindowLayout::Full { height } => {
                let main = Container::new(&self.sweep_view);
                let preview = Container::new(self.large_preview.as_ref());
                if height.is_positive() {
                    FlexRef::column((FlexChild::new(main), FlexChild::new(preview)))
                        .render(ctx, surf, layout)
                } else {
                    FlexRef::column((FlexChild::new(preview), FlexChild::new(main)))
                        .render(ctx, surf, layout)
                }
            }
        }
    }

    fn layout(
        &self,
        ctx: &ViewContext,
        ct: BoxConstraint,
        mut layout: ViewMutLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        match &self.sweep_layout {
            WindowLayout::Float {
                height,
                width,
                row,
                column,
            } => {
                let mut pos = self.term_position;
                if !row.is_full() {
                    pos.row = row.calc(ct.max().height);
                }
                pos.col = column.calc(ct.max().width);
                let size = Size {
                    height: height.calc(ct.max().height),
                    width: width.calc(ct.max().width).min(ct.max().width - pos.col),
                };
                pos.row = pos.row.min(ct.max().height - size.height);
                let mut child_layout = layout.push_default();
                self.sweep_view
                    .layout(ctx, BoxConstraint::tight(size), child_layout.view_mut())?;
                child_layout.set_position(pos);
                layout.set_size(ct.max());
            }
            WindowLayout::Full { height } => {
                let sweep_height = height.calc(ct.max().height);
                let sweep = Container::new(&self.sweep_view);
                let preview =
                    Container::new(self.large_preview.as_ref()).with_vertical(Align::Expand);
                let view = if height.is_positive() {
                    FlexRef::column((
                        FlexChild::new(sweep.with_height(sweep_height)),
                        FlexChild::new(preview.with_height(ct.max().height - sweep_height)),
                    ))
                    .left_view()
                } else {
                    FlexRef::column((
                        FlexChild::new(preview.with_height(sweep_height)),
                        FlexChild::new(sweep.with_height(ct.max().height - sweep_height)),
                    ))
                    .right_view()
                };
                view.layout(ctx, ct, layout)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum WindowLayoutSize {
    Absolute(i32),
    Fraction(f32),
    Full,
}

impl WindowLayoutSize {
    fn calc(&self, size: usize) -> usize {
        match *self {
            WindowLayoutSize::Absolute(diff) => {
                if diff >= 0 {
                    (diff as usize).clamp(0, size)
                } else {
                    size - (-diff as usize).clamp(0, size)
                }
            }
            WindowLayoutSize::Fraction(frac) => {
                if frac >= 0.0 {
                    ((size as f32 * frac) as usize).clamp(0, size)
                } else {
                    size - ((size as f32 * -frac) as usize).clamp(0, size)
                }
            }
            WindowLayoutSize::Full => size,
        }
    }

    fn is_full(&self) -> bool {
        matches!(self, WindowLayoutSize::Full)
    }

    fn is_positive(&self) -> bool {
        match *self {
            WindowLayoutSize::Absolute(val) => val >= 0,
            WindowLayoutSize::Fraction(val) => val >= 0.0,
            WindowLayoutSize::Full => true,
        }
    }
}

impl std::str::FromStr for WindowLayoutSize {
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
    id: SweepItemId,
    theme: Theme,
    preview: P,
    height: Arc<AtomicUsize>,
}

impl<P> SweepPreview<P> {
    fn new(id: SweepItemId, theme: Theme, preview: P) -> Self {
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

        Flex::row()
            .add_flex_child(1.0, preview)
            .add_child(scrollbar)
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
