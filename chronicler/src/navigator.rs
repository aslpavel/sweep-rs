use crate::{
    history::{History, HistoryEntry},
    utils::AbortJoinHandle,
    walk::{path_ignore_for_path, walk, PathItem},
};
use anyhow::Error;
use async_trait::async_trait;
use futures::{future, stream, Stream, StreamExt, TryStreamExt};
use std::{
    collections::HashMap,
    fmt,
    io::Write,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
};
use sweep::{
    surf_n_term::{Glyph, Key},
    Haystack, HaystackPreview, Positions, Sweep, SweepEvent, SweepOptions,
};

#[derive(Debug, Clone)]
pub enum NavigatorItem {
    Path(PathItem),
    History(HistoryEntry),
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
    type Context = ();

    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char),
    {
        use NavigatorItem::*;
        match self {
            Path(path) => path.haystack_scope(scope),
            History(history) => history.haystack_scope(scope),
        }
    }

    fn view(
        &self,
        ctx: &Self::Context,
        positions: &sweep::Positions,
        theme: &sweep::Theme,
    ) -> Box<dyn sweep::surf_n_term::view::View> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.view(ctx, positions, theme),
            History(history) => history.view(ctx, positions, theme),
        }
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        positions: &Positions,
        theme: &sweep::Theme,
    ) -> Option<HaystackPreview> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.preview(ctx, positions, theme),
            History(history) => history.preview(ctx, positions, theme),
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
        let sweep = Sweep::new((), options)?;
        sweep.scorer_by_name(Some("substr".to_owned())).await?;
        sweep.bind(
            vec!["tab".parse()?],
            TAG_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+i".parse()?],
            TAG_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+r".parse()?],
            TAG_COMMAND_HISTORY_MODE.to_owned(),
            "Switch to command history view".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+f".parse()?],
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
        self.sweep.items_clear();
        let sweep = self.sweep.clone();
        self.update_task = Some(
            tokio::spawn(async move {
                if let Err(error) = sweep.items_extend_stream(items).await {
                    tracing::error!(?error, "[Navigator.list_update]");
                };
            })
            .into(),
        );
    }

    async fn path_complete(&self) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        let (current, query) =
            tokio::try_join!(self.sweep.items_current(), self.sweep.query_get())?;

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
            self.sweep.query_set(query);
        }
        while let Some(event) = self.sweep.next_event().await {
            match event {
                SweepEvent::Resize(_) => {}
                SweepEvent::Select(result) => return Ok(result),
                SweepEvent::Bind(tag) => {
                    let mode_next = match tag.as_str() {
                        TAG_COMMAND_HISTORY_MODE => Some(CmdHistoryMode::new(None)),
                        TAG_PATH_HISTORY_MODE => Some(PathHistoryMode::new()),
                        _ => mode.handler(self, tag).await?,
                    };
                    if let Some(mode_next) = mode_next {
                        mode.exit(self).await?;
                        mode = mode_next;
                        mode.enter(self).await?;
                    }
                }
            }
        }
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait NavigatorMode {
    /// Enter mode
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error>;

    /// Destroy mode
    async fn exit(&mut self, navigator: &mut Navigator) -> Result<(), Error>;

    /// Handle key binding
    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: String,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error>;
}

pub struct CmdHistoryMode {
    session: Option<String>,
}

impl CmdHistoryMode {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(session: Option<String>) -> Box<dyn NavigatorMode> {
        Box::new(Self { session })
    }
}

#[async_trait]
impl NavigatorMode for CmdHistoryMode {
    async fn enter(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator
            .sweep
            .prompt_set(Some("CMD".to_owned()), Some(CMD_HISTORY_ICON.clone()));
        navigator.sweep.keep_order(Some(true));
        navigator.sweep.bind(
            Key::chord("alt+g s")?,
            TAG_GOTO_SESSION.to_owned(),
            "Go to parent directory".to_owned(),
        );
        navigator.sweep.bind(
            Key::chord("alt+g d")?,
            TAG_GOTO_DIRECTORY.to_owned(),
            "Go to current working directory of the command".to_owned(),
        );

        // NOTE: I have not found a way to create static stream from connection
        //       pool even though it is clone-able.
        let history = if let Some(session) = &self.session {
            navigator
                .history
                .entries_session(session.clone())
                .map_ok(NavigatorItem::History)
                .collect::<Vec<_>>()
                .await
        } else {
            navigator
                .history
                .entries_unique_cmd()
                .map_ok(NavigatorItem::History)
                .collect::<Vec<_>>()
                .await
        };
        navigator.list_update(stream::iter(history));
        Ok(())
    }

    async fn exit(&mut self, navigator: &mut Navigator) -> Result<(), Error> {
        navigator
            .sweep
            .bind(Key::chord("alt+g s")?, String::new(), String::new());
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: String,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        let current = navigator.sweep.items_current().await?;
        let Some(NavigatorItem::History(entry)) = current else {
            return Ok(None);
        };
        match tag.as_str() {
            TAG_GOTO_SESSION => Ok(Some(CmdHistoryMode::new(Some(entry.session)))),
            TAG_GOTO_DIRECTORY => Ok(Some(PathMode::new(entry.cwd.into(), String::new()))),
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
        navigator
            .sweep
            .prompt_set(Some(path_collapse(&self.path)), Some(PATH_NAV_ICON.clone()));
        navigator.sweep.keep_order(Some(true));
        navigator.sweep.query_set(self.query.clone());

        navigator.sweep.bind(
            vec!["backspace".parse()?],
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
            .bind(vec!["backspace".parse()?], String::new(), String::new());
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: String,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        match tag.as_str() {
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
        navigator
            .sweep
            .prompt_set(Some("PATH".to_owned()), Some(PATH_HISTORY_ICON.clone()));
        navigator.sweep.keep_order(Some(true));

        let mut history = Vec::new();
        // Add current directory as the first item
        let current_dir = std::env::current_dir();
        if let Ok(current_dir) = &current_dir {
            history.push(NavigatorItem::Path(PathItem {
                root_length: 0,
                path: current_dir.clone(),
                metadata: None,
                ignore: None,
            }));
        };
        let current_dir = current_dir.unwrap_or_default();
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
                        }))
                    }
                }
                future::ready(())
            })
            .await;
        navigator.list_update(stream::iter(history).map(Ok));

        Ok(())
    }

    async fn exit(&mut self, _navigator: &mut Navigator) -> Result<(), Error> {
        Ok(())
    }

    async fn handler(
        &mut self,
        navigator: &mut Navigator,
        tag: String,
    ) -> Result<Option<Box<dyn NavigatorMode>>, Error> {
        match tag.as_str() {
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

lazy_static::lazy_static! {
    static ref ICONS: HashMap<String, Glyph> =
        serde_json::from_str(include_str!("./icons.json"))
            .expect("invalid icons.json file");

    static ref PATH_HISTORY_ICON: &'static Glyph = ICONS.get("material-folder-clock-outline")
        .expect("failed to find path history icon");

    static ref PATH_NAV_ICON: &'static Glyph = ICONS.get("material-folder-search-outline")
        .expect("failed to find path navigation icon");

    static ref CMD_HISTORY_ICON: &'static Glyph = ICONS.get("material-console")
        .expect("failed to find path navigation icon");

    pub(crate) static ref FAILED_ICON: &'static Glyph = ICONS.get("material-close-circle-outline")
        .expect("faield to fined failed icon");
}

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
