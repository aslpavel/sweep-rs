use super::DATE_FORMAT;
use crate::navigator::NavigatorContext;
use anyhow::Error;
use futures::{
    ready,
    stream::{self, FuturesOrdered},
    FutureExt, Stream, StreamExt, TryStreamExt,
};
use globset::{GlobBuilder, GlobMatcher};
use std::{
    collections::{HashSet, VecDeque},
    fmt::{self, Write},
    fs::Metadata,
    future::Future,
    ops::Deref,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    task::Poll,
};
use sweep::{
    surf_n_term::{
        view::{BoxView, Flex, Justify, Text, View},
        Face, FaceAttrs,
    },
    Haystack,
};
use time::OffsetDateTime;
use tokio::fs;
use tokio_stream::wrappers::ReadDirStream;

#[derive(Clone)]
pub struct PathItem {
    /// Number of bytes in the path that are part of the root of the walk
    pub root_length: usize,
    /// Full path of an item
    pub path: PathBuf,
    /// Metadata associated with path
    pub metadata: Option<Metadata>,
    /// Ignore matcher
    pub ignore: Option<PathIgnoreArc>,
    /// Number of visits (coming from history)
    pub visits: Option<i64>,
}

impl PathItem {
    pub fn is_dir(&self) -> bool {
        self.metadata.as_ref().map_or_else(|| false, |m| m.is_dir())
    }

    pub async fn unfold(&self) -> Result<Vec<PathItem>, Error> {
        if !self.is_dir() {
            return Ok(Vec::new());
        }

        let ignore: Option<PathIgnoreArc> =
            match PathIgnoreGit::new(self.path.join(".gitignore")).await {
                Err(_) => self.ignore.clone(),
                Ok(git_ignore) => {
                    if let Some(ignore) = self.ignore.clone() {
                        Some(Arc::new(git_ignore.chain(ignore.clone())))
                    } else {
                        Some(Arc::new(git_ignore))
                    }
                }
            };

        let read_dir = fs::read_dir(&self.path).await?;
        let mut entries: Vec<_> = ReadDirStream::new(read_dir)
            .map_ok(|entry| entry.path())
            .try_filter_map(|path| async {
                let metadata = fs::symlink_metadata(&path).await.ok();
                let path_item = PathItem {
                    path,
                    metadata,
                    root_length: self.root_length,
                    ignore: ignore.clone(),
                    visits: None,
                };
                if ignore
                    .as_ref()
                    .map_or_else(|| false, |ignore| ignore.matches(&path_item))
                {
                    return Ok(None);
                }
                Ok(Some(path_item))
            })
            .try_collect()
            .await?;

        entries.sort_unstable_by(|a, b| path_sort_key(b).cmp(&path_sort_key(a)));
        Ok(entries)
    }
}

impl fmt::Debug for PathItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PathItem")
            .field("root_length", &self.root_length)
            .field("path", &self.path)
            .field("metadata", &self.metadata)
            .finish()
    }
}

impl Haystack for PathItem {
    type Context = NavigatorContext;

    fn haystack_scope<S>(&self, ctx: &Self::Context, mut scope: S)
    where
        S: FnMut(char),
    {
        let path = self.path.to_string_lossy();
        let home_dir_len = ctx.home_dir.chars().count();
        let skip = if !ctx.home_dir.is_empty()
            && self.root_length < home_dir_len
            && path.starts_with(ctx.home_dir.deref())
        {
            scope('~');
            home_dir_len - self.root_length
        } else {
            0
        };
        match path.get(self.root_length..) {
            Some(path) => path.chars().skip(skip).for_each(&mut scope),
            None => path.chars().skip(skip).for_each(&mut scope),
        }
        if self.is_dir() {
            scope('/')
        }
    }

    fn view(
        &self,
        ctx: &Self::Context,
        positions: &sweep::Positions,
        theme: &sweep::Theme,
    ) -> BoxView<'static> {
        let path = sweep::haystack_default_view(ctx, self, positions, theme);
        let mut right = Text::new();
        right.set_face(theme.list_inactive);
        if let Some(visits) = self.visits {
            write!(&mut right, "{visits} ").expect("in memory write failed");
        }
        Flex::row()
            .justify(Justify::SpaceBetween)
            .add_child(path)
            .add_child(right)
            .boxed()
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        _positions: &sweep::Positions,
        _theme: &sweep::Theme,
    ) -> Option<sweep::HaystackPreview> {
        let metadata = self.metadata.as_ref()?;
        let left_face = Some(Face::default().with_attrs(FaceAttrs::BOLD));
        let mut text = Text::new()
            .push_str("Mode     ", left_face)
            .push_fmt(format_args!("{}\n", unix_mode::to_string(metadata.mode())))
            .push_str("Size     ", left_face)
            .push_fmt(format_args!("{:.2}\n", SizeDisplay::new(metadata.len())))
            .take();
        text.push_str("Owner    ", left_face);
        match ctx.get_user_by_uid(metadata.uid()) {
            None => text.push_fmt(format_args!("{}", metadata.uid())),
            Some(user) => text.push_fmt(format_args!("{}", user.name().to_string_lossy())),
        };
        text.push_str(":", None);
        match ctx.get_group_by_gid(metadata.gid()) {
            None => text.push_fmt(format_args!("{}", metadata.uid())),
            Some(group) => text.push_fmt(format_args!("{}", group.name().to_string_lossy())),
        };
        text.push_str("\n", None);
        if let Ok(created) = metadata.created() {
            let date = OffsetDateTime::from(created).format(&DATE_FORMAT).ok()?;
            text.push_str("Created  ", left_face);
            text.push_fmt(format_args!("{}\n", date));
        }
        if let Ok(modified) = metadata.modified() {
            let date = OffsetDateTime::from(modified).format(&DATE_FORMAT).ok()?;
            text.push_str("Modified ", left_face);
            text.push_fmt(format_args!("{}\n", date));
        }
        if let Ok(accessed) = metadata.accessed() {
            let date = OffsetDateTime::from(accessed).format(&DATE_FORMAT).ok()?;
            text.push_str("Accessed ", left_face);
            text.push_fmt(format_args!("{}\n", date));
        }
        Some(sweep::HaystackPreview::new(text.boxed(), None))
    }
}

/// Walk directory returning a stream of [PathItem] in the breadth first order
pub fn walk<'caller>(
    root: impl AsRef<Path> + 'caller,
    ignore: Option<PathIgnoreArc>,
) -> impl Stream<Item = Result<PathItem, Error>> + 'caller {
    let root = root
        .as_ref()
        .canonicalize()
        .unwrap_or_else(|_| root.as_ref().to_owned());
    fs::symlink_metadata(root.to_owned())
        .then(move |metadata| async move {
            let root_length = root.as_os_str().len() + 1;
            let init = PathItem {
                root_length,
                path: root,
                metadata: metadata.ok(),
                ignore,
                visits: None,
            };
            bounded_unfold(64, Some(init), |item| async move {
                let children = match item.unfold().await {
                    Ok(children) => children,
                    Err(error) => {
                        tracing::warn!(?item.path, ?error, "[walk]");
                        Vec::new()
                    }
                };
                Ok((item, children))
            })
        })
        .into_stream()
        .flatten()
}

fn path_sort_key(item: &PathItem) -> (bool, bool, &Path) {
    let hidden = item
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| false, |name| name.starts_with('.'));
    let is_dir = item
        .metadata
        .as_ref()
        .map_or_else(|| false, |meta| meta.is_dir());
    (hidden, !is_dir, &item.path)
}

/// Format size in a human readable form
pub struct SizeDisplay {
    size: u64,
}

impl SizeDisplay {
    pub fn new(size: u64) -> Self {
        Self { size }
    }
}

impl std::fmt::Display for SizeDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.size < 1024 {
            return write!(f, "{}B", self.size);
        }
        let mut size = self.size as f64;
        let precision = f.precision().unwrap_or(1);
        for mark in "KMGTP".chars() {
            size /= 1024.0;
            if size < 1024.0 {
                return write!(f, "{0:.1$}{2}", size, precision, mark);
            }
        }
        write!(f, "{0:.1$}P", size, precision)
    }
}

pub trait PathIgnore {
    fn matches(&self, item: &PathItem) -> bool;

    fn chain<O>(self, other: O) -> PathIgnoreChain<Self, O>
    where
        O: PathIgnore + Sized,
        Self: Sized,
    {
        PathIgnoreChain {
            first: self,
            second: other,
        }
    }
}

pub struct PathIgnoreChain<F, S> {
    first: F,
    second: S,
}

impl<F, S> PathIgnore for PathIgnoreChain<F, S>
where
    F: PathIgnore,
    S: PathIgnore,
{
    fn matches(&self, item: &PathItem) -> bool {
        self.first.matches(item) || self.second.matches(item)
    }
}

pub type PathIgnoreArc = Arc<dyn PathIgnore + Send + Sync + 'static>;

impl PathIgnore for PathIgnoreArc {
    fn matches(&self, item: &PathItem) -> bool {
        (**self).matches(item)
    }
}

struct GlobGit {
    matcher: GlobMatcher,
    /// match on filename only
    is_filename: bool,
    /// match on directory only
    is_dir: bool,
    /// match is a whitelist
    is_whitelist: bool,
}

impl GlobGit {
    fn new(string: &str) -> impl Iterator<Item = GlobGit> + '_ {
        string.lines().filter_map(|line| {
            let line = line.trim();
            // comments
            if line.starts_with('#') || line.is_empty() {
                return None;
            }

            // directory only match
            let (is_dir, line) = if line.ends_with('/') {
                (true, line.trim_end_matches('/'))
            } else {
                (false, line)
            };

            // whitelist
            let (is_whitelist, line) = if line.starts_with('!') {
                (true, line.trim_start_matches('!'))
            } else {
                (false, line)
            };

            // filename match
            let is_filename = !line.contains('/');
            let line = line.trim_start_matches('/');

            if let Ok(glob) = GlobBuilder::new(line).literal_separator(true).build() {
                Some(GlobGit {
                    matcher: glob.compile_matcher(),
                    is_dir,
                    is_filename,
                    is_whitelist,
                })
            } else {
                None
            }
        })
    }
}

struct PathIgnoreGit {
    positive: Vec<GlobGit>,
    negative: Vec<GlobGit>,
    root: PathBuf,
}

impl PathIgnoreGit {
    async fn new(path: impl AsRef<Path>) -> Result<Self, Error> {
        let data = fs::read(path.as_ref()).await?;
        let (positive, negative) =
            GlobGit::new(std::str::from_utf8(&data)?).partition(|glob| !glob.is_whitelist);
        Ok(Self {
            positive,
            negative,
            root: path
                .as_ref()
                .parent()
                .unwrap_or(Path::new(""))
                .canonicalize()?,
        })
    }
}

impl PathIgnore for PathIgnoreGit {
    fn matches(&self, item: &PathItem) -> bool {
        let Ok(path) = item.path.strip_prefix(&self.root) else {
            return false;
        };
        let filename = Path::new(
            path.file_name()
                .and_then(|path| path.to_str())
                .unwrap_or(""),
        );
        let is_dir = item.is_dir();

        for glob in &self.negative {
            if glob.is_dir && !is_dir {
                continue;
            }
            let matched = if glob.is_filename {
                glob.matcher.is_match(filename)
            } else {
                glob.matcher.is_match(path)
            };
            if matched {
                return false;
            }
        }

        for glob in &self.positive {
            if glob.is_dir && !is_dir {
                continue;
            }
            let matched = if glob.is_filename {
                glob.matcher.is_match(filename)
            } else {
                glob.matcher.is_match(path)
            };
            if matched {
                return true;
            }
        }
        false
    }
}

pub struct PathIgnoreSet {
    pub filenames: HashSet<String>,
}

impl Default for PathIgnoreSet {
    fn default() -> Self {
        Self {
            filenames: [".hg", ".git"].iter().map(|f| f.to_string()).collect(),
        }
    }
}

impl PathIgnore for PathIgnoreSet {
    fn matches(&self, item: &PathItem) -> bool {
        let filename = item
            .path
            .file_name()
            .and_then(|path| path.to_str())
            .unwrap_or("");
        self.filenames.contains(filename)
    }
}

pub async fn path_ignore_for_path(path: impl AsRef<Path>) -> PathIgnoreArc {
    let mut ignore: PathIgnoreArc = Arc::new(PathIgnoreSet::default());
    for path in path.as_ref().ancestors() {
        if let Ok(git_ignore) = PathIgnoreGit::new(path.join(".gitignore")).await {
            ignore = Arc::new(git_ignore.chain(ignore));
        }
    }
    ignore
}

/// Similar to unfold but runs unfold function in parallel with the specified
/// level of parallelism
pub fn bounded_unfold<'caller, In, Init, Ins, Out, Unfold, UFut, UErr>(
    scheduled_max: usize,
    init: Init,
    unfold: Unfold,
) -> impl Stream<Item = Result<Out, UErr>> + 'caller
where
    In: 'caller,
    Out: 'caller,
    Unfold: Fn(In) -> UFut + 'caller,
    UErr: 'caller,
    UFut: Future<Output = Result<(Out, Ins), UErr>> + 'caller,
    Init: IntoIterator<Item = In> + 'caller,
    Ins: IntoIterator<Item = In> + 'caller,
{
    let mut unscheduled = VecDeque::from_iter(init);
    let mut scheduled = FuturesOrdered::new();
    stream::poll_fn(move |cx| loop {
        if scheduled.is_empty() && unscheduled.is_empty() {
            return Poll::Ready(None);
        }

        for item in
            unscheduled.drain(..std::cmp::min(unscheduled.len(), scheduled_max - scheduled.len()))
        {
            scheduled.push_back(unfold(item))
        }

        if let Some((out, children)) = ready!(scheduled.poll_next_unpin(cx)).transpose()? {
            for child in children {
                unscheduled.push_front(child);
            }
            return Poll::Ready(Some(Ok(out)));
        }
    })
}
