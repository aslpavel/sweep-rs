use crate::{
    history::{History, HistoryEntry},
    utils::AbortJoinHandle,
    walk::{path_ignore_for_path, walk, PathItem},
};
use anyhow::Error;
use futures::{future, stream, Stream, StreamExt, TryStreamExt};
use std::{
    collections::HashMap,
    fmt,
    io::Write,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    str::FromStr,
};
use sweep::{
    surf_n_term::Glyph, Haystack, HaystackPreview, Positions, Sweep, SweepEvent, SweepOptions,
};

#[derive(Debug, Clone)]
pub enum NavigatorItem {
    Path(PathItem),
    History(HistoryEntry),
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

#[derive(Debug, Clone)]
pub enum NavigatorState {
    /// Show specified path
    Path(PathBuf),
    /// Show command history
    CmdHistory,
    /// Show path history
    PathHistory,
}

pub struct Navigator {
    sweep: Sweep<NavigatorItem>,
    history: History,
    state: NavigatorState,
    update_task: Option<AbortJoinHandle<()>>,
}

impl Navigator {
    pub async fn new(
        options: SweepOptions,
        db_path: impl AsRef<Path>,
        state: NavigatorState,
    ) -> Result<Self, Error> {
        let sweep = Sweep::new((), options)?;
        NavigatorBind::bind(&sweep)?;
        Ok(Self {
            sweep,
            history: History::new(db_path).await?,
            state,
            update_task: None,
        })
    }

    async fn switch_mode(
        &mut self,
        state: NavigatorState,
        query: Option<&str>,
    ) -> Result<(), Error> {
        use NavigatorState::*;
        if let Some(query) = query {
            self.sweep.query_set(query);
        }
        self.state = state;
        match &self.state {
            Path(path) => {
                self.sweep
                    .prompt_set(Some(path_collapse(path)), Some(PATH_NAV_ICON.clone()));
                self.list_update(
                    walk(
                        path.clone(),
                        Some(path_ignore_for_path(path.as_path()).await),
                    )
                    .try_filter(|item| {
                        future::ready(item.path.as_os_str().len() >= item.root_length)
                    })
                    .map_ok(NavigatorItem::Path),
                );
            }
            CmdHistory => {
                self.sweep
                    .prompt_set(Some("CMD".to_owned()), Some(CMD_HISTORY_ICON.clone()));
                // NOTE: I have not found a way to create static stream from connection
                //       pool even though it is clone-able.
                let history = self
                    .history
                    .entries_unique_cmd()
                    .map_ok(NavigatorItem::History)
                    .collect::<Vec<_>>()
                    .await;
                self.list_update(stream::iter(history));
            }
            PathHistory => {
                self.sweep
                    .prompt_set(Some("PATH".to_owned()), Some(PATH_HISTORY_ICON.clone()));
                let history = self
                    .history
                    .path_entries()
                    .map_ok(|item| {
                        NavigatorItem::Path(PathItem {
                            root_length: 0,
                            path: item.path.into(),
                            metadata: None,
                            ignore: None,
                        })
                    })
                    .collect::<Vec<_>>()
                    .await;
                self.list_update(stream::iter(history))
            }
        }
        Ok(())
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

    // Go to parent directory
    async fn cmd_goto_parent(&mut self) -> Result<(), Error> {
        use NavigatorState::*;
        if let Path(path) = &self.state {
            if let Some(path) = path.parent().map(PathBuf::from) {
                self.switch_mode(Path(path), Some("")).await?;
            }
        }
        Ok(())
    }

    // Complete either with current item of or current query
    async fn cmd_complete(&mut self) -> Result<(), Error> {
        if matches!(
            &self.state,
            NavigatorState::Path(_) | NavigatorState::PathHistory
        ) {
            let (current, query) =
                tokio::try_join!(self.sweep.items_current(), self.sweep.query_get())?;
            if query.starts_with('~') || query.starts_with('/') || current.is_none() {
                let (path, query) = get_path_and_query(query).await;
                self.switch_mode(NavigatorState::Path(path), Some(query.as_ref()))
                    .await?;
            } else if let Some(NavigatorItem::Path(item)) = current {
                let is_dir = if let Some(metadata) = item.metadata {
                    metadata.is_dir()
                } else {
                    tokio::fs::metadata(&item.path)
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false)
                };
                if is_dir {
                    self.switch_mode(NavigatorState::Path(item.path), Some(""))
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub async fn run(&mut self, query: Option<&str>) -> Result<Option<NavigatorItem>, Error> {
        self.switch_mode(self.state.clone(), query).await?;
        while let Some(event) = self.sweep.next_event().await {
            match event {
                SweepEvent::Select(result) => return Ok(result),
                SweepEvent::Bind(bind) => match bind.parse()? {
                    NavigatorBind::GotoParent => self.cmd_goto_parent().await?,
                    NavigatorBind::Completion => self.cmd_complete().await?,
                    NavigatorBind::ShowCommandHistory => {
                        self.switch_mode(NavigatorState::CmdHistory, Some(""))
                            .await?
                    }
                    NavigatorBind::ShowPathHistory => {
                        self.switch_mode(NavigatorState::PathHistory, Some(""))
                            .await?
                    }
                },
                SweepEvent::Resize(_) => {}
            }
        }
        Ok(None)
    }
}

const CMD_GOTO_PARENT: &str = "chronicler.path.goto.parent";
const CMD_COMPLETE: &str = "chronicler.path.complete";
const CMD_PATH_HISTORY: &str = "chronicler.path.history";
const CMD_COMMAND_HISTORY: &str = "chronicler.cmd.history";

enum NavigatorBind {
    GotoParent,
    Completion,
    ShowCommandHistory,
    ShowPathHistory,
}

impl NavigatorBind {
    fn bind(sweep: &Sweep<NavigatorItem>) -> Result<(), Error> {
        sweep.bind(
            vec!["backspace".parse()?],
            CMD_GOTO_PARENT.to_owned(),
            "Go to parent directory".to_owned(),
        );
        sweep.bind(
            vec!["tab".parse()?],
            CMD_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+i".parse()?],
            CMD_COMPLETE.to_owned(),
            "Complete string or follow directory".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+r".parse()?],
            CMD_COMMAND_HISTORY.to_owned(),
            "Switch to command history view".to_owned(),
        );
        sweep.bind(
            vec!["ctrl+f".parse()?],
            CMD_PATH_HISTORY.to_owned(),
            "Switch to path history view".to_owned(),
        );
        Ok(())
    }
}

impl FromStr for NavigatorBind {
    type Err = Error;

    fn from_str(bind: &str) -> Result<Self, Self::Err> {
        match bind {
            CMD_GOTO_PARENT => Ok(Self::GotoParent),
            CMD_COMPLETE => Ok(Self::Completion),
            CMD_COMMAND_HISTORY => Ok(Self::ShowCommandHistory),
            CMD_PATH_HISTORY => Ok(Self::ShowPathHistory),
            cmd => Err(anyhow::anyhow!("unhandled bind command: {}", cmd)),
        }
    }
}

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
        write!(&mut result, "/\u{2026}")?;
        for part in parts[parts.len() - 4..].iter() {
            write!(&mut result, "/")?;
            result.write_all(part.as_bytes())?;
        }
        Ok::<_, Error>(())
    })()
    .expect("in memory write failed");
    String::from_utf8_lossy(result.as_slice()).into_owned()
}
