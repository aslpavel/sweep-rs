use crate::{
    history::{History, HistoryEntry},
    utils::AbortJoinHandle,
    walk::{walk, PathItem},
};
use anyhow::Error;
use futures::{stream, Stream, StreamExt, TryStreamExt};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};
use sweep::{surf_n_term::Glyph, Haystack, Sweep, SweepEvent, SweepOptions};

const CMD_GOTO_PARENT: &str = "path.goto.parent";
const CMD_COMPLETE: &str = "path.complete";
const CMD_PATH_HISTORY: &str = "path.history";
const CMD_COMMAND_HISTORY: &str = "cmd.hisotory";

#[derive(Debug, Clone)]
pub enum NavigatorItem {
    Path(PathItem),
    History(HistoryEntry),
}

impl Haystack for NavigatorItem {
    fn haystack(&self) -> Box<dyn Iterator<Item = char> + '_> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.haystack(),
            History(history) => history.haystack(),
        }
    }

    fn view(
        &self,
        positions: &sweep::Positions,
        theme: &sweep::Theme,
        refs: sweep::FieldRefs,
    ) -> Box<dyn sweep::surf_n_term::view::View> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.view(positions, theme, refs),
            History(history) => history.view(positions, theme, refs),
        }
    }

    fn preview(&self, theme: &sweep::Theme) -> Option<Box<dyn sweep::surf_n_term::view::View>> {
        use NavigatorItem::*;
        match self {
            Path(path) => path.preview(theme),
            History(history) => history.preview(theme),
        }
    }
}

impl fmt::Display for NavigatorItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NavigatorItem::Path(path) => write!(f, "{:?}", path.path),
            NavigatorItem::History(entry) => write!(f, "{}", entry.cmd),
        }
    }
}

#[derive(Debug, Clone)]
pub enum NavigatorState {
    Path(PathBuf),
    CmdHistory,
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
        let sweep = Sweep::new(options)?;
        sweep.bind(vec!["backspace".parse()?], CMD_GOTO_PARENT.to_string());
        sweep.bind(vec!["tab".parse()?], CMD_COMPLETE.to_string());
        sweep.bind(vec!["ctrl+i".parse()?], CMD_COMPLETE.to_string());
        sweep.bind(vec!["ctrl+r".parse()?], CMD_COMMAND_HISTORY.to_string());
        sweep.bind(vec!["ctrl+f".parse()?], CMD_PATH_HISTORY.to_string());
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
                // TODO:
                // - collapse path (replace home and add ellipsis if needed)
                self.sweep.prompt_set(
                    Some(path.to_string_lossy().into_owned()),
                    Some(PATH_NAV_ICON.clone()),
                );
                self.list_update(walk(path.clone(), |_| false).map_ok(NavigatorItem::Path));
            }
            CmdHistory => {
                self.sweep
                    .prompt_set(Some("CMD".to_owned()), Some(CMD_HISTORY_ICON.clone()));
                // NOTE: I have not found a way to create static stream from connection
                //       pool even though it is clone-able.
                let history = self
                    .history
                    .entries()
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
                            path: item.path.into(),
                            metadata: None,
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
                    tracing::error!(?error, "failed to generate items");
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

    pub async fn run(&mut self) -> Result<Option<NavigatorItem>, Error> {
        self.switch_mode(self.state.clone(), None).await?;
        while let Some(event) = self.sweep.event().await {
            match event {
                SweepEvent::Select(result) => return Ok(result),
                SweepEvent::Bind(bind) => match bind.as_str() {
                    CMD_GOTO_PARENT => self.cmd_goto_parent().await?,
                    CMD_COMPLETE => self.cmd_complete().await?,
                    CMD_COMMAND_HISTORY => {
                        self.switch_mode(NavigatorState::CmdHistory, Some(""))
                            .await?
                    }
                    CMD_PATH_HISTORY => {
                        self.switch_mode(NavigatorState::PathHistory, Some(""))
                            .await?
                    }
                    cmd => return Err(anyhow::anyhow!("unhandled bind command: {}", cmd)),
                },
            }
        }
        Ok(None)
    }
}

lazy_static::lazy_static! {
    static ref PATH_HISTORY_ICON: Glyph = {
        use sweep::surf_n_term::{BBox, Path, FillRule, Size};
        let path: Path = "
            M15,12H16.5V16.25L19.36,17.94L18.61,19.16L15,17V12M19,8H3V18H9.29
            C9.1,17.37 9,16.7 9,16A7,7 0 0,1 16,9C17.07,9 18.09,9.24 19,9.67V8
            M3,20C1.89,20 1,19.1 1,18V6A2,2 0 0,1 3,4H9L11,6H19A2,2 0 0,1 21,8
            V11.1C22.24,12.36 23,14.09 23,16A7,7 0 0,1 16,23C13.62,23 11.5,21.81 10.25,20
            H3M16,11A5,5 0 0,0 11,16A5,5 0 0,0 16,21A5,5 0 0,0 21,16A5,5 0 0,0 16,11Z
        "
            .parse()
            .expect("failed to parse path history icon");
        Glyph::new(
            Arc::new(path),
            FillRule::default(),
            Some(BBox::new((0.0, 0.0), (24.0, 24.0))),
            Size::new(1, 3),
        )
    };

    static ref PATH_NAV_ICON: Glyph = {
        use sweep::surf_n_term::{BBox, Path, FillRule, Size};
        let path: Path = "
            M16.5,12C19,12 21,14 21,16.5C21,17.38 20.75,18.21 20.31,18.9L23.39,22
            L22,23.39L18.88,20.32C18.19,20.75 17.37,21 16.5,21C14,21 12,19 12,16.5
            C12,14 14,12 16.5,12M16.5,14A2.5,2.5 0 0,0 14,16.5A2.5,2.5 0 0,0 16.5,19
            A2.5,2.5 0 0,0 19,16.5A2.5,2.5 0 0,0 16.5,14M19,8H3V18H10.17
            C10.34,18.72 10.63,19.39 11,20H3C1.89,20 1,19.1 1,18V6C1,4.89 1.89,4 3,4
            H9L11,6H19A2,2 0 0,1 21,8V11.81C20.42,11.26 19.75,10.81 19,10.5V8Z
        "
            .parse()
            .expect("failed to parse path history icon");
        Glyph::new(
            Arc::new(path),
            FillRule::default(),
            Some(BBox::new((0.0, 0.0), (24.0, 24.0))),
            Size::new(1, 3),
        )
    };

    static ref CMD_HISTORY_ICON: Glyph = {
        use sweep::surf_n_term::{BBox, Path, FillRule, Size};
        let path: Path = "
            M20,19V7H4V19H20M20,3A2,2 0 0,1 22,5V19A2,2 0 0,1 20,21H4
            A2,2 0 0,1 2,19V5C2,3.89 2.9,3 4,3H20M13,17V15H18V17H13M9.58,13
            L5.57,9H8.4L11.7,12.3C12.09,12.69 12.09,13.33 11.7,13.72L8.42,17
            H5.59L9.58,13Z
        "
            .parse()
            .expect("failed to parse path history icon");
        Glyph::new(
            Arc::new(path),
            FillRule::default(),
            Some(BBox::new((0.0, 0.0), (24.0, 24.0))),
            Size::new(1, 3),
        )
    };
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
