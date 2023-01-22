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
    pub path: PathBuf,
    pub metadata: Option<Metadata>,
}

impl Haystack for PathItem {
    fn haystack(&self) -> Box<dyn Iterator<Item = char> + '_> {
        let chars: Vec<_> = self.path.to_string_lossy().chars().collect();
        Box::new(chars.into_iter())
    }
}

/// Walk directory returning a stream of [PathItem] in the breadth first order
pub fn walk<'caller>(
    root: impl AsRef<Path> + 'caller,
    ignore: impl Fn(&Path) -> bool + 'caller,
) -> impl Stream<Item = Result<PathItem, Error>> + 'caller {
    let ignore = Arc::new(ignore);
    fs::symlink_metadata(root.as_ref().to_owned())
        .then(|metadata| async move {
            let init = PathItem {
                path: root.as_ref().to_owned(),
                metadata: metadata.ok(),
            };
            bounded_unfold(64, Some(init), move |item| {
                let ignore = ignore.clone();
                async move {
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
                                    Ok(Some(PathItem { path, metadata }))
                                })
                                .try_collect()
                                .await?;
                            entries
                                .sort_unstable_by(|a, b| path_sort_key(b).cmp(&path_sort_key(a)));
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
