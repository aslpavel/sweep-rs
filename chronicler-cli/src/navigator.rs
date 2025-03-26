use crate::{
    history::{History, HistoryEntry},
    walk::{PathItem, path_ignore_for_path, walk},
};
use anyhow::Error;
use async_trait::async_trait;
use futures::{Stream, StreamExt, TryStreamExt, future, stream};
use std::{
    collections::HashMap,
    fmt,
    io::Write,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, RwLock},
};
use sweep::{
    Haystack, Positions, Sweep, SweepEvent, SweepOptions,
    common::{AbortJoinHandle, LockExt},
    surf_n_term::{
        Glyph,
        view::{Either, View},
    },
};

#[derive(Debug, Clone)]
pub enum NavigatorItem {
    Path(PathItem),
    History(HistoryEntry),
}

#[derive(Debug, Clone)]
pub struct NavigatorContext {
    pub cwd: Arc<str>,
    pub home_dir: Arc<str>,
    users_cache: Arc<RwLock<HashMap<u32, Option<uzers::User>>>>,
    groups_cache: Arc<RwLock<HashMap<u32, Option<uzers::Group>>>>,
}

impl NavigatorContext {
    pub fn get_group_by_gid(&self, gid: u32) -> Option<uzers::Group> {
        match self.groups_cache.with(|groups| groups.get(&gid).cloned()) {
            Some(group) => group,
            None => {
                let group = uzers::get_group_by_gid(gid);
                self.groups_cache
                    .with_mut(|groups| groups.insert(gid, group.clone()));
                group
            }
        }
    }

    pub fn get_user_by_uid(&self, uid: u32) -> Option<uzers::User> {
        match self.users_cache.with(|users| users.get(&uid).cloned()) {
            Some(user) => user,
            None => {
                let user = uzers::get_user_by_uid(uid);
                self.users_cache
                    .with_mut(|users| users.insert(uid, user.clone()));
                user
            }
        }
    }
}

impl NavigatorItem {
    pub fn tag(&self) -> &str {
        match self {
            NavigatorItem::History(_) => "R", // run
            NavigatorItem::Path(entry) => {
                let is_dir = entry
                    .metadata
                    .as_ref()
                    .map(|m| m.is_dir())
                    .or_else(|| Some(entry.path.metadata().ok()?.is_dir()))
                    .unwrap_or(false);
                if is_dir {
                    "D" // dir
                } else {
                    "F" // file
                }
            }
        }
    }
}

impl Haystack for NavigatorItem {
    type Context = NavigatorContext;
    type View = Either<<PathItem as Haystack>::View, <HistoryEntry as Haystack>::View>;
    type Preview = Either<<PathItem as Haystack>::Preview, <HistoryEntry as Haystack>::Preview>;
    type PreviewLarge =
        Either<<PathItem as Haystack>::PreviewLarge, <HistoryEntry as Haystack>::PreviewLarge>;

    fn haystack_scope<S>(&self, ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        use NavigatorItem::*;
        match self {
            Path(path) => path.haystack_scope(ctx, scope),
            History(history) => history.haystack_scope(ctx, scope),
        }
    }

    fn view(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &sweep::Theme,
    ) -> Self::View {
        use NavigatorItem::*;
        match self {
            Path(path) => path.view(ctx, positions, theme).left_view(),
            History(history) => history.view(ctx, positions, theme).right_view(),
        }
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &sweep::Theme,
    ) -> Option<Self::Preview> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.preview(ctx, positions, theme).map(Either::Left),
            History(history) => history.preview(ctx, positions, theme).map(Either::Right),
        }
    }

    fn preview_large(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &sweep::Theme,
    ) -> Option<Self::PreviewLarge> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.preview_large(ctx, positions, theme).map(Either::Left),
            History(history) => history
                .preview_large(ctx, positions, theme)
                .map(Either::Right),
        }
    }
}

impl fmt::Display for NavigatorItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NavigatorItem::Path(path) => write!(f, "{}", path.path.to_string_lossy()),
            NavigatorItem::History(entry) => write!(f, "{}", entry.cmd),
        }
    }
}

pub struct Navigator {
    sweep: Sweep<NavigatorItem>,
    history: History,
    update_task: Option<AbortJoinHandle<()>>,
}

impl Navigator {
    pub async fn new(options: SweepOptions, db_path: impl AsRef<Path>) -> Result<Self, Error> {
        let ctx = NavigatorContext {
            cwd: std::env::current_dir()
                .map_or_else(
                    |_| String::new(),
                    |path| path.to_string_lossy().into_owned(),
                )
                .into(),
            home_dir: dirs::home_dir()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned())
                .into(),
            users_cache: Default::default(),
            groups_cache: Default::default(),
        };
        let sweep = Sweep::new(ctx, options)?;
        sweep
            .scorer_by_name(None, Some("substr".to_owned()))
            .await?;
        sweep.bind(
            None,
            "tab".parse()?,
            TAG_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            None,
            "ctrl+i".parse()?,
            TAG_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            None,
            "ctrl+r".parse()?,
            TAG_COMMAND_HISTORY_MODE.to_owned(),
            "Switch to command history view".to_owned(),
        );
        sweep.bind(
            None,
            "ctrl+f".parse()?,
            TAG_PATH_HISTORY_MODE.to_owned(),
            "Switch to path history view".to_owned(),
        );

        Ok(Self {
            sweep,
            history: History::new(db_path).await?,
            update_task: None,
        })
    }

    // Abort current item list update and start a new one
    fn list_update(
        &mut self,
        items: impl Stream<Item = Result<NavigatorItem, Error>> + Send + 'static,
    ) {
        if let Some(update_task) = self.update_task.take() {
            update_task.abort();
        }
        self.sweep.items_clear(None);
        let sweep = self.sweep.clone();
        self.update_task = Some(
            tokio::spawn(async move {
                if let Err(error) = sweep.items_extend_stream(None, items).await {
                    tracing::error!(?error, "[Navigator.list_update]");
                };
            })
            .into(),
        );
    }

    async fn path_complete(&self) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        let (current, query) =
            tokio::try_join!(self.sweep.items_current(None), self.sweep.query_get(None))?;

        if query.starts_with('~') || query.starts_with('/') {
            // navigate path from query string
            let (path, query) = get_path_and_query(query).await;
            Ok(Some(PathMode::new(path, query)))
        } else if let Some(NavigatorItem::Path(path_item)) = current {
            // navigate to currently pointed directory
            let is_dir = if let Some(metadata) = path_item.metadata {
                metadata.is_dir()
            } else {
                tokio::fs::metadata(&path_item.path)
                    .await
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
            };
            if is_dir {
                Ok(Some(PathMode::new(path_item.path, String::new())))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    pub async fn run(
        &mut self,
        query: Option<&str>,
        mut mode: Box<dyn NavigatorMode>,
    ) -> Result<Vec<NavigatorItem>, Error> {
        mode.enter(self).await?;
        if let Some(query) = query {
            self.sweep.query_set(None, query);
        }
        while let Some(event) = self.sweep.next_event().await {
            match event {
                SweepEvent::Resize(_) => {}
                SweepEvent::Select { items, .. } => return Ok(items),
                SweepEvent::Bind { tag, .. } => {
                    tracing::debug!(?tag, "[Navigator.run]");
                    let mode_next = match tag.as_ref() {
                        TAG_COMMAND_HISTORY_MODE => Some(CmdHistoryMode::new(None, None)),
                        TAG_PATH_HISTORY_MODE => Some(PathHistoryMode::new()),
                        _ => mode.handler(self, tag).await?,
                    };
                    if let Some(mode_next) = mode_next {
                        mode.exit(self).await?;
                        mode = mode_next;
                        mode.enter(self).await?;
                    }
                }
                SweepEvent::Window { .. } => {}
            }
        }
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait NavigatorMode {
    // fn uid(&self) -> WindowId;

    /// Enter mode
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error>;

    /// Destroy mode
    async fn exit(&mut self, navigator: &mut Navigator) -> Result<(), Error>;

    /// Handle key binding
    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: Arc<str>,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error>;
}

pub struct CmdHistoryMode {
    session: Option<String>,
    filter_path: Option<String>,
}

impl CmdHistoryMode {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(session: Option<String>, filter_path: Option<String>) -> Box<dyn NavigatorMode> {
        Box::new(Self {
            session,
            filter_path,
        })
    }
}

#[async_trait]
impl NavigatorMode for CmdHistoryMode {
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator
            .sweep
            .prompt_set(None, Some("CMD".to_owned()), Some(CMD_HISTORY_ICON.clone()));
        navigator.sweep.keep_order(None, Some(true));
        navigator.sweep.bind(
            None,
            "alt+g s".parse()?,
            TAG_GOTO_SESSION.to_owned(),
            "Go to session of the current command".to_owned(),
        );
        navigator.sweep.bind(
            None,
            "alt+g d".parse()?,
            TAG_GOTO_DIRECTORY.to_owned(),
            "Go to current working directory of the command".to_owned(),
        );
        navigator.sweep.bind(
            None,
            "alt+g c".parse()?,
            TAG_FILTER_CWD.to_owned(),
            "Keep only commands that were executed in the current directory".to_owned(),
        );

        let history = if let Some(session) = &self.session {
            navigator.history.entries_session(session.clone()).boxed()
        } else {
            navigator.history.entries_unique_cmd().boxed()
        };
        let history = if let Some(filter_path) = self.filter_path.clone() {
            tracing::debug!(?filter_path, "[CmdHistoryMode.enter] filter path");
            history
                .try_filter(move |entry| future::ready(entry.cwd == filter_path))
                .boxed()
        } else {
            history
        };

        // keep commands executed in the current directory at the top
        let cwd = std::env::current_dir()
            .map_or_else(|_| String::new(), |cwd| cwd.to_string_lossy().into_owned());
        let mut history = history.collect::<Vec<_>>().await;
        history.sort_by_key(|entry| {
            entry
                .as_ref()
                .map_or_else(|_| true, |entry| entry.cwd != cwd)
        });

        navigator.list_update(stream::iter(
            history
                .into_iter()
                .map(|entry| entry.map(NavigatorItem::History)),
        ));
        Ok(())
    }

    async fn exit(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator
            .sweep
            .bind(None, "alt+g s".parse()?, String::new(), String::new());
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: Arc<str>,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        match tag.as_ref() {
            TAG_GOTO_SESSION => {
                let session = if self.session.is_none() {
                    let current = navigator.sweep.items_current(None).await?;
                    let Some(NavigatorItem::History(entry)) = current else {
                        return Ok(None);
                    };
                    Some(entry.session)
                } else {
                    None
                };
                Ok(Some(CmdHistoryMode::new(session, None)))
            }
            TAG_FILTER_CWD => {
                let dir = if self.filter_path.is_none() {
                    std::env::current_dir()
                        .ok()
                        .map(|dir| dir.to_string_lossy().into_owned())
                } else {
                    None
                };
                Ok(Some(CmdHistoryMode::new(self.session.clone(), dir)))
            }
            TAG_GOTO_DIRECTORY => {
                let current = navigator.sweep.items_current(None).await?;
                let Some(NavigatorItem::History(entry)) = current else {
                    return Ok(None);
                };
                Ok(Some(PathMode::new(entry.cwd.into(), String::new())))
            }
            _ => Ok(None),
        }
    }
}

pub struct PathMode {
    path: PathBuf,
    query: String,
}

impl PathMode {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(path: PathBuf, query: String) -> Box<dyn NavigatorMode> {
        Box::new(Self { path, query })
    }
}

#[async_trait]
impl NavigatorMode for PathMode {
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator.sweep.prompt_set(
            None,
            Some(path_collapse(&self.path)),
            Some(PATH_NAV_ICON.clone()),
        );
        navigator.sweep.keep_order(None, Some(true));
        navigator.sweep.query_set(None, self.query.clone());

        navigator.sweep.bind(
            None,
            "backspace".parse()?,
            TAG_GOTO_PARENT.to_owned(),
            "Go to parent directory".to_owned(),
        );

        navigator.list_update(
            walk(
                self.path.clone(),
                Some(path_ignore_for_path(self.path.as_path()).await),
            )
            .try_filter(|item| future::ready(item.path.as_os_str().len() >= item.root_length))
            .map_ok(NavigatorItem::Path),
        );

        Ok(())
    }

    async fn exit(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator
            .sweep
            .bind(None, "backspace".parse()?, String::new(), String::new());
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: Arc<str>,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        match tag.as_ref() {
            TAG_COMPLETE => navigator.path_complete().await,
            TAG_GOTO_PARENT => {
                if let Some(path) = self.path.parent().map(PathBuf::from) {
                    if self.path != path {
                        return Ok(Some(PathMode::new(path, String::new())));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

pub struct PathHistoryMode {}

impl PathHistoryMode {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Box<dyn NavigatorMode> {
        Box::new(Self {})
    }
}

#[async_trait]
impl NavigatorMode for PathHistoryMode {
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator.sweep.prompt_set(
            None,
            Some("PATH".to_owned()),
            Some(PATH_HISTORY_ICON.clone()),
        );
        navigator.sweep.keep_order(None, Some(true));

        let mut history = Vec::new();
        // Add current directory as the first item
        let current_dir = std::env::current_dir();
        if let Ok(current_dir) = &current_dir {
            history.push(NavigatorItem::Path(PathItem {
                root_length: 0,
                path: current_dir.clone(),
                metadata: None,
                ignore: None,
                visits: None,
            }));
        };
        let current_dir = current_dir.unwrap_or_default();
        let mut current_visits = 0;
        navigator
            .history
            .path_entries()
            .for_each(|item| {
                if let Ok(item) = item {
                    let path: PathBuf = item.path.into();
                    if path != current_dir {
                        history.push(NavigatorItem::Path(PathItem {
                            root_length: 0,
                            path,
                            metadata: None,
                            ignore: None,
                            visits: Some(item.count),
                        }))
                    } else {
                        current_visits = item.count;
                    }
                }
                future::ready(())
            })
            .await;
        if let Some(NavigatorItem::Path(entry)) = history.get_mut(0) {
            entry.visits = Some(current_visits);
        }
        navigator.list_update(stream::iter(history).map(Ok));

        Ok(())
    }

    async fn exit(&mut self, _navigator: &mut Navigator) -> Result<(), Error> {
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: Arc<str>,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        match tag.as_ref() {
            TAG_COMPLETE => navigator.path_complete().await,
            _ => Ok(None),
        }
    }
}

const TAG_PATH_HISTORY_MODE: &str = "chronicler.mode.path";
const TAG_COMMAND_HISTORY_MODE: &str = "chronicler.mode.cmd";
const TAG_COMPLETE: &str = "chronicler.complete";
const TAG_GOTO_PARENT: &str = "chronicler.goto.parent";
const TAG_GOTO_SESSION: &str = "chronicler.goto.session";
const TAG_GOTO_DIRECTORY: &str = "chronicler.goto.directory";
const TAG_FILTER_CWD: &str = "chronicler.filter.cwd";

static ICONS: LazyLock<HashMap<String, Glyph>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("./icons.json")).expect("invalid icons.json file")
});
static PATH_HISTORY_ICON: LazyLock<&'static Glyph> = LazyLock::new(|| {
    ICONS
        .get("path-history")
        .expect("failed to find path history icon")
});
static PATH_NAV_ICON: LazyLock<&'static Glyph> = LazyLock::new(|| {
    ICONS
        .get("path-navigation")
        .expect("failed to find path navigation icon")
});
static CMD_HISTORY_ICON: LazyLock<&'static Glyph> = LazyLock::new(|| {
    ICONS
        .get("cmd-history")
        .expect("failed to find path navigation icon")
});
pub(crate) static FAILED_ICON: LazyLock<&'static Glyph> =
    LazyLock::new(|| ICONS.get("failed").expect("faield to find failed icon"));
pub(crate) static FOLDER_ICON: LazyLock<&'static Glyph> =
    LazyLock::new(|| ICONS.get("folder").expect("faield to find folder icon"));

/// Find longest existing path from the input and use reminder as query
async fn get_path_and_query(input: impl AsRef<str>) -> (PathBuf, String) {
    // expand homedir
    let input = input.as_ref();
    let path = if let Ok(suffix) = Path::new(input).strip_prefix("~/") {
        dirs::home_dir().map_or_else(|| Path::new(input).into(), |home| home.join(suffix))
    } else {
        Path::new(input).into()
    };
    // search for longest existing prefix directory
    for ancestor in path.ancestors() {
        let is_dir = tokio::fs::metadata(ancestor)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            let query = path
                .strip_prefix(ancestor)
                .unwrap_or_else(|_| Path::new(""))
                .to_str()
                .unwrap_or("")
                .to_owned();
            return (ancestor.to_owned(), query);
        }
    }
    (PathBuf::new(), input.to_owned())
}

/// Collapse long path with ellipsis, and replace home directory with ~
fn path_collapse(path: &Path) -> String {
    // replace home directory with ~
    let path = (|| {
        let home_dir = dirs::home_dir()?;
        let path = path.canonicalize().ok()?;
        Some(Path::new("~").join(path.strip_prefix(home_dir).ok()?))
    })()
    .unwrap_or(path.to_owned());

    // return already short path
    if path.iter().count() <= 5 {
        return path.to_string_lossy().into_owned();
    }

    // shorten path
    let parts: Vec<_> = path.iter().collect();
    let mut result: Vec<u8> = Vec::new();
    (|| {
        result.write_all(parts[0].as_bytes())?;
        if !path.has_root() {
            write!(&mut result, "/")?;
        }
        write!(&mut result, "\u{2026}")?;
        for part in parts[parts.len() - 4..].iter() {
            write!(&mut result, "/")?;
            result.write_all(part.as_bytes())?;
        }
        Ok::<_, Error>(())
    })()
    .expect("in memory write failed");
    String::from_utf8_lossy(result.as_slice()).into_owned()
}
