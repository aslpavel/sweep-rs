use anyhow::Error;
use futures::{
    ready,
    stream::{self, FuturesOrdered},
    FutureExt, Stream, StreamExt, TryStreamExt,
};
use std::{
    collections::VecDeque,
    fs::Metadata,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    task::Poll,
};
use sweep::Haystack;
use tokio::fs;
use tokio_stream::wrappers::ReadDirStream;

#[derive(Debug, Clone)]
pub struct PathItem {
    /// Number of bytes in the path that are part of the root of the walk
    pub root_length: usize,
    /// Full path of an item
    pub path: PathBuf,
    /// Metadata associated with path
    pub metadata: Option<Metadata>,
}

impl Haystack for PathItem {
    fn haystack_scope<S>(&self, mut scope: S)
    where
        S: FnMut(char),
    {
        let path = self.path.to_string_lossy();
        match path.get(self.root_length..) {
            Some(path) => path.chars().for_each(&mut scope),
            None => path.chars().for_each(&mut scope),
        }
        if let Some(true) = self.metadata.as_ref().map(|m| m.is_dir()) {
            scope('/')
        }
    }
}

/// Walk directory returning a stream of [PathItem] in the breadth first order
pub fn walk<'caller>(
    root: impl AsRef<Path> + 'caller,
    ignore: impl Fn(&Path) -> bool + 'caller,
) -> impl Stream<Item = Result<PathItem, Error>> + 'caller {
    let ignore = Arc::new(ignore);
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
            };
            bounded_unfold(64, Some(init), move |item| {
                path_unfold(item, ignore.clone())
            })
        })
        .into_stream()
        .flatten()
}

async fn path_unfold<I>(item: PathItem, ignore: Arc<I>) -> Result<(PathItem, Vec<PathItem>), Error>
where
    I: Fn(&Path) -> bool,
{
    let children = match &item.metadata {
        Some(metadata) if metadata.is_dir() => async {
            let read_dir = fs::read_dir(&item.path).await?;
            let mut entries: Vec<_> = ReadDirStream::new(read_dir)
                .map_ok(|entry| entry.path())
                .try_filter_map(|path| async {
                    if ignore(&path) {
                        return Ok(None);
                    }
                    let metadata = fs::symlink_metadata(&path).await.ok();
                    Ok(Some(PathItem {
                        root_length: item.root_length,
                        path,
                        metadata,
                    }))
                })
                .try_collect()
                .await?;
            entries.sort_unstable_by(|a, b| path_sort_key(b).cmp(&path_sort_key(a)));
            Ok::<_, Error>(entries)
        }
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(?item.path, ?error, "failed to list directory");
            Vec::new()
        }),
        _ => Vec::new(),
    };
    Ok((item, children))
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
