use flume::RecvError;
use std::{
    future::Future,
    hash::Hash,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::Poll,
};
use tokio::{fs::File, io::AsyncWrite, sync::Semaphore, task::JoinSet};

use crate::SyncError;

#[non_exhaustive]
#[derive(Debug, Default)]
/// Global progress tracking.
#[allow(missing_docs)]
pub struct GlobalProgress {
    pub files: ProgressTIDSF<AtomicU64>,
    pub bytes: ProgressTIDSF<AtomicU64>,
}

#[derive(Debug, Clone, Copy)]
/// Progress milestones.
pub enum ProgressMilestone {
    /// Discovery phase is complete, the total number of files and bytes is known.
    DiscoveryComplete,
    /// Copy phase is complete.
    CopyComplete,
}

#[derive(Debug, Default, Clone, Copy)]
/// Progress tracking for a single file.
#[allow(missing_docs)]
pub struct FileProgress {
    pub total: u64,
    pub done: u64,
}

/// A structure for tracking progress where the total, in progress, done, skipped, and failed counts are tracked.
pub struct TrackingAsyncWrite<'a, W: AsyncWrite, K: Unpin, F: Fn(&K, &FileProgress)> {
    job_id: K,
    progress_callback: &'a F,
    size: u64,
    fp: FileProgress,
    gp: &'a GlobalProgress,
    failed: bool,
    finalized: bool,
    written: u64,
    last_progress_reported: u64,
    inner: Pin<&'a mut W>,
}

impl<'a, W: AsyncWrite, K: Unpin, F: Fn(&K, &FileProgress)> TrackingAsyncWrite<'a, W, K, F> {
    /// Create a new `TrackingAsyncWrite` instance.
    pub fn new(
        job_id: K,
        size: u64,
        gp: &'a GlobalProgress,
        progress_callback: &'a F,
        inner: Pin<&'a mut W>,
    ) -> Self {
        gp.files.in_progress.fetch_add(1, Ordering::Relaxed);
        let fp = FileProgress {
            total: size,
            done: 0,
        };
        progress_callback(&job_id, &fp);
        Self {
            job_id,
            progress_callback,
            size,
            inner,
            gp,
            failed: false,
            finalized: false,
            written: 0,
            last_progress_reported: 0,
            fp,
        }
    }

    fn register_fail(&mut self) {
        if !self.failed {
            self.gp.bytes.failed.fetch_add(self.size, Ordering::Relaxed);
            self.gp.files.in_progress.fetch_sub(1, Ordering::Relaxed);
            self.gp.files.failed.fetch_add(1, Ordering::Relaxed);
            self.failed = true;
        }
    }

    fn increment_bytes(&mut self, n: u64) {
        if !self.failed {
            self.written += n;
            if self.written - self.last_progress_reported >= 64 << 10 {
                (self.progress_callback)(&self.job_id, &self.fp);
                self.last_progress_reported = self.written;
            }
            self.fp.done += n;
            self.gp.bytes.in_progress.fetch_add(n, Ordering::Relaxed);
        }
    }

    fn finalize(&mut self) {
        (self.progress_callback)(&self.job_id, &self.fp);
        if !self.failed && !self.finalized {
            if self.written != self.size {
                self.register_fail();
            }
            self.gp
                .bytes
                .done
                .fetch_add(self.written, Ordering::Relaxed);
            self.gp
                .bytes
                .in_progress
                .fetch_sub(self.size, Ordering::Relaxed);
            self.gp.files.in_progress.fetch_sub(1, Ordering::Relaxed);
            self.gp.files.done.fetch_add(1, Ordering::Relaxed);
            self.finalized = true;
        }
    }

    fn revert_progress(&mut self) {
        if !self.failed && self.finalized {
            self.gp
                .bytes
                .done
                .fetch_sub(self.written, Ordering::Relaxed);
            self.gp.files.done.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl<'a, W: AsyncWrite, K: Unpin, F: Fn(&K, &FileProgress)> AsyncWrite
    for TrackingAsyncWrite<'a, W, K, F>
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.inner.as_mut().poll_write(cx, buf) {
            Poll::Ready(r) => match r {
                Err(e) => {
                    self.register_fail();
                    Poll::Ready(Err(e))
                }
                Ok(n) => {
                    self.increment_bytes(n as u64);
                    Poll::Ready(Ok(n))
                }
            },
            r => r,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.inner.as_mut().poll_flush(cx) {
            Poll::Ready(r) => match r {
                Err(e) => {
                    self.register_fail();
                    Poll::Ready(Err(e))
                }
                Ok(_) => Poll::Ready(Ok(())),
            },
            r => r,
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.inner.as_mut().poll_shutdown(cx) {
            Poll::Ready(r) => match r {
                Err(e) => {
                    self.register_fail();
                    Poll::Ready(Err(e))
                }
                Ok(_) => {
                    self.finalize();
                    Poll::Ready(Ok(()))
                }
            },
            r => r,
        }
    }
}

impl<'a, W: AsyncWrite, K: Unpin, F: Fn(&K, &FileProgress)> Drop
    for TrackingAsyncWrite<'a, W, K, F>
{
    fn drop(&mut self) {
        self.finalize();
    }
}

#[derive(Debug, Default)]
/// A structure for tracking progress where the total, in progress, done, skipped, and failed counts are tracked.
#[allow(missing_docs)]
pub struct ProgressTIDSF<T: Default> {
    pub total: T,
    pub in_progress: T,
    pub done: T,
    pub skipped: T,
    pub failed: T,
}

/// A structure for synchronizing two directories.
pub struct SyncFS<'a> {
    src_root: &'a PathBuf,
    dest_root: &'a PathBuf,
    ctx: Arc<SyncFSCtx>,
}

struct SyncFSCtx {
    progress: GlobalProgress,
    semaphore: Semaphore,
}

impl<'a> SyncFS<'a> {
    /// Create a new `SyncFS` instance.
    pub fn new(src_root: &'a PathBuf, dest_root: &'a PathBuf, max_concurrent: usize) -> Self {
        log::info!(
            "Creating SyncFS instance from {} to {}, concurrency: {}",
            src_root.display(),
            dest_root.display(),
            max_concurrent
        );
        Self {
            ctx: Arc::new(SyncFSCtx {
                progress: GlobalProgress::default(),
                semaphore: Semaphore::new(max_concurrent),
            }),
            src_root,
            dest_root,
        }
    }
    fn walk(
        &'a self,
        rel: PathBuf,
        tx: &'a flume::Sender<Result<(PathBuf, PathBuf), SyncError>>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            let src = self.src_root.join(&rel);
            let dest = self.dest_root.join(&rel);

            let src_meta = match tokio::fs::metadata(&src).await {
                Ok(m) => m,
                Err(e) => {
                    tx.send_async(Err(SyncError::StatFailed(src.clone(), e)))
                        .await
                        .expect("Result receiver dropped");
                    return;
                }
            };

            if src_meta.is_file() {
                self.ctx
                    .progress
                    .files
                    .total
                    .fetch_add(1, Ordering::Relaxed);
                self.ctx
                    .progress
                    .bytes
                    .total
                    .fetch_add(src_meta.len(), Ordering::Relaxed);

                if !cmp_file(dest.clone(), src.clone()).await.unwrap_or(false) {
                    if let Err(e) = tx.send_async(Ok((src.clone(), dest.clone()))).await {
                        log::error!("Failed to send copy job: {}", e);
                    }
                } else {
                    self.ctx
                        .progress
                        .files
                        .skipped
                        .fetch_add(1, Ordering::Relaxed);
                    self.ctx
                        .progress
                        .bytes
                        .skipped
                        .fetch_add(src_meta.len(), Ordering::Relaxed);
                }
            } else if src_meta.is_dir() {
                match tokio::fs::create_dir_all(&dest).await {
                    Ok(_) => {}
                    Err(e) => {
                        tx.send_async(Err(SyncError::CopyFailed {
                            src: src.clone(),
                            dest,
                            err: e,
                        }))
                        .await
                        .expect("Result receiver dropped");
                        return;
                    }
                }
                let mut rd = match tokio::fs::read_dir(&src).await {
                    Ok(rd) => rd,
                    Err(e) => {
                        tx.send_async(Err(SyncError::StatFailed(src.clone(), e)))
                            .await
                            .expect("Result receiver dropped");
                        return;
                    }
                };
                loop {
                    match rd.next_entry().await {
                        Err(e) => {
                            tx.send_async(Err(SyncError::StatFailed(src.clone(), e)))
                                .await
                                .expect("Result receiver dropped");
                            return;
                        }
                        Ok(None) => break,
                        Ok(Some(entry)) => {
                            self.walk(rel.join(entry.file_name()), tx).await;
                        }
                    }
                }
            }
        })
    }
    /// Synchronize the two directories, the Future will resolve when the synchronization is complete.
    ///
    /// Progress will be periodically reported to the `progress_fn` callback.
    /// Errors will be reported to the `error_fn` callback.
    pub async fn sync<F: Fn(&GlobalProgress, Option<ProgressMilestone>), EF: Fn(&SyncError)>(
        &self,
        progress_fn: F,
        error_fn: &EF,
    ) {
        let (tx, rx) = flume::bounded(2048);

        let mut js = JoinSet::new();

        tokio::join!(async move { self.walk(PathBuf::new(), &tx).await }, async {
            loop {
                match rx.recv_async().await {
                    Ok(Ok((src, dest))) => {
                        let ctx_clone = self.ctx.clone();
                        js.spawn(async move {
                            copy_file(
                                src.clone(),
                                dest.clone(),
                                src.clone(),
                                Some(&ctx_clone.semaphore),
                                &ctx_clone.progress,
                                &|k, prog| {
                                    println!("File: {:?} - {}/{}", k, prog.done, prog.total);
                                },
                            )
                            .await
                            .map(|_| (src, dest))
                        });
                    }
                    Ok(Err(e)) => {
                        println!("Error occurred during discovery: {}", e);
                        error_fn(&e);
                        self.ctx
                            .progress
                            .files
                            .total
                            .fetch_add(1, Ordering::Relaxed);
                        self.ctx
                            .progress
                            .files
                            .failed
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Err(RecvError::Disconnected) => {
                        return;
                    }
                }
            }
        });

        progress_fn(
            &self.ctx.progress,
            Some(ProgressMilestone::DiscoveryComplete),
        );

        let total = js.len();
        let one_pct = std::cmp::max(1, total / 100);
        let mut last_reported = 0;
        let mut completed = 0;

        while let Some(result) = js.join_next().await {
            completed += 1;
            if completed - last_reported >= one_pct {
                progress_fn(&self.ctx.progress, None);
                last_reported = js.len();
            }

            match result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    println!("Error occurred during copy: {}", e);
                    continue;
                }
                Err(e) => {
                    if e.is_cancelled() {
                        error_fn(&SyncError::Cancelled);
                    } else {
                        error_fn(&SyncError::JoinError(e));
                    }
                }
            }
        }

        progress_fn(&self.ctx.progress, Some(ProgressMilestone::CopyComplete));
    }
}

async fn cmp_file(dest: PathBuf, src: PathBuf) -> Result<bool, tokio::io::Error> {
    let dest_meta = tokio::fs::metadata(&dest).await?;
    let src_meta = tokio::fs::metadata(&src).await?;

    if dest_meta.len() != src_meta.len() {
        return Ok(false);
    }

    if dest_meta.modified()? < src_meta.modified()? {
        return Ok(false);
    }

    Ok(true)
}

async fn copy_file<K: Hash + PartialEq + Unpin, F: Fn(&K, &FileProgress)>(
    job_id: K,
    dest: PathBuf,
    src: PathBuf,
    semaphore: Option<&Semaphore>,
    progress: &GlobalProgress,
    file_progress_callback: &F,
) -> Result<u64, SyncError> {
    let permit = match semaphore {
        Some(s) => match s.acquire().await {
            Ok(p) => Some(p),
            Err(_) => {
                progress.files.failed.fetch_add(1, Ordering::Relaxed);
                return Err(SyncError::Cancelled);
            }
        },
        None => None,
    };

    let mut src_file = match File::open(&src).await {
        Ok(f) => f,
        Err(e) => {
            progress.files.failed.fetch_add(1, Ordering::Relaxed);
            return Err(SyncError::CopyFailed {
                src: src.clone(),
                dest,
                err: e,
            });
        }
    };

    let src_meta = src_file.metadata().await.map_err(|e| {
        progress.files.failed.fetch_add(1, Ordering::Relaxed);
        SyncError::StatFailed(src.clone(), e)
    })?;

    let dst_file = std::pin::pin!(match File::create(&dest).await {
        Ok(f) => f,
        Err(e) => {
            progress.files.failed.fetch_add(1, Ordering::Relaxed);
            return Err(SyncError::CopyFailed { src, dest, err: e });
        }
    });

    let mut dest_write = TrackingAsyncWrite::new(
        job_id,
        src_meta.len(),
        progress,
        file_progress_callback,
        dst_file,
    );

    // This already handles flushing the file so we don't need to do it again.
    let result = tokio::io::copy(&mut src_file, &mut dest_write).await;

    drop(permit);

    match result {
        Ok(written) => {
            if written != src_meta.len() {
                dest_write.revert_progress();
                progress.files.failed.fetch_add(1, Ordering::Relaxed);
                progress
                    .bytes
                    .failed
                    .fetch_add(src_meta.len(), Ordering::Relaxed);
                return Err(SyncError::ShortCopy {
                    src,
                    dest,
                    copied: written,
                    expected: src_meta.len(),
                });
            }
            Ok(written)
        }
        Err(e) => {
            progress.files.failed.fetch_add(1, Ordering::Relaxed);
            Err(SyncError::CopyFailed { src, dest, err: e })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_copy_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let src = tmp_dir.path().join("src");
        let dest = tmp_dir.path().join("dest");

        let mut src_file = File::create(&src).await.unwrap();
        src_file.write_all(b"hello world").await.unwrap();

        copy_file(
            "test",
            dest.clone(),
            src.clone(),
            None,
            &GlobalProgress::default(),
            &|_, _| {},
        )
        .await
        .unwrap();

        let mut dest_file = File::open(&dest).await.unwrap();
        let mut buf = Vec::new();
        dest_file.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, b"hello world");
    }

    #[tokio::test]
    async fn test_sync() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let src = tmp_dir.path().join("src");
        let dest = tmp_dir.path().join("dest");

        let src_subdir = src.join("subdir");
        let dest_subdir = dest.join("subdir");
        tokio::fs::create_dir_all(&src_subdir).await.unwrap();

        let src_file = src.join("file");
        let dest_file = dest.join("file");
        tokio::fs::write(&src_file, b"hello world").await.unwrap();

        let src_subfile = src_subdir.join("subfile");
        let dest_subfile = dest_subdir.join("subfile");
        tokio::fs::write(&src_subfile, b"goodbye world")
            .await
            .unwrap();

        let sync = SyncFS::new(&src, &dest, 1);

        let done = AtomicU64::new(0);

        sync.sync(
            |gp, _| {
                done.store(gp.files.done.load(Ordering::Relaxed), Ordering::Relaxed);
            },
            &|e| {
                panic!("Error occurred: {:?}", e);
            },
        )
        .await;

        assert_eq!(done.into_inner(), 2);

        let mut dest_file = File::open(&dest_file).await.unwrap();

        let mut buf = Vec::new();
        dest_file.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, b"hello world");

        let mut dest_subfile = File::open(&dest_subfile).await.unwrap();

        let mut buf = Vec::new();
        dest_subfile.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, b"goodbye world");
    }
}
