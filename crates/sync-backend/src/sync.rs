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
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt, BufWriter},
    sync::Semaphore,
    task::JoinSet,
};

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
pub struct SyncFS {
    src_root: PathBuf,
    dest_root: PathBuf,
    ctx: Arc<SyncFSCtx>,
}

struct SyncFSCtx {
    progress: GlobalProgress,
    semaphore: Semaphore,
}

impl SyncFS {
    /// Create a new `SyncFS` instance.
    pub fn new(src_root: PathBuf, dest_root: PathBuf, max_concurrent: usize) -> Self {
        Self {
            ctx: Arc::new(SyncFSCtx {
                progress: GlobalProgress::default(),
                semaphore: Semaphore::new(max_concurrent),
            }),
            src_root,
            dest_root,
        }
    }
    fn walk<'a>(
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

                        self.ctx.progress.files.done.fetch_add(1, Ordering::Relaxed);
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

async fn copy_file<K: Hash + PartialEq, F: Fn(&K, &FileProgress)>(
    job_id: K,
    dest: PathBuf,
    src: PathBuf,
    semaphore: Option<&Semaphore>,
    progress: &GlobalProgress,
    file_progress_callback: &F,
) -> Result<(), SyncError> {
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

    let mut buf = vec![0u8; 128 << 10];

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
    let mut file_progress = FileProgress {
        total: src_meta.len(),
        ..Default::default()
    };
    file_progress_callback(&job_id, &file_progress);

    let dst_file = match File::create(&dest).await {
        Ok(f) => f,
        Err(e) => {
            progress.files.failed.fetch_add(1, Ordering::Relaxed);
            return Err(SyncError::CopyFailed { src, dest, err: e });
        }
    };

    let mut dest_write = BufWriter::new(dst_file);

    progress.files.in_progress.fetch_add(1, Ordering::Relaxed);

    let result = loop {
        let n = match src_file.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(e),
        };
        progress
            .bytes
            .in_progress
            .fetch_add(n as u64, Ordering::Relaxed);
        file_progress.done += n as u64;
        file_progress_callback(&job_id, &file_progress);
        match dest_write.write_all(&buf[..n]).await {
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };

    progress.files.in_progress.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok(()) => {
            dest_write.flush().await.map_err(|e| {
                progress.files.failed.fetch_add(1, Ordering::Relaxed);
                SyncError::CopyFailed {
                    src: src.clone(),
                    dest: dest.clone(),
                    err: e,
                }
            })?;
            progress
                .bytes
                .done
                .fetch_add(file_progress.total, Ordering::Relaxed);
            progress
                .bytes
                .in_progress
                .fetch_sub(file_progress.total, Ordering::Relaxed);
            drop(permit);

            Ok(())
        }
        Err(e) => {
            progress.files.failed.fetch_add(1, Ordering::Relaxed);
            progress
                .bytes
                .failed
                .fetch_add(file_progress.total, Ordering::Relaxed);
            progress.files.in_progress.fetch_sub(1, Ordering::Relaxed);
            drop(permit);

            Err(SyncError::CopyFailed { src, dest, err: e })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cmp_file() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let src = tmp_dir.path().join("src");
        let dest = tmp_dir.path().join("dest");

        let mut src_file = File::create(&src).await.unwrap();
        src_file.write_all(b"hello world").await.unwrap();

        let mut dest_file = File::create(&dest).await.unwrap();
        dest_file.write_all(b"hello world").await.unwrap();

        assert!(cmp_file(src.clone(), dest.clone()).await.unwrap());

        src_file.write_all(b"HELLO world").await.unwrap();

        assert!(!cmp_file(src.clone(), dest.clone()).await.unwrap());
    }

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

        let sync = SyncFS::new(src.clone(), dest.clone(), 1);

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
