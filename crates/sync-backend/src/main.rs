use clap::Parser;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use indicatif::{MultiProgress, ProgressBar};
use sync_backend::{
    sync::{ProgressMilestone, SyncFS},
    Config,
};
use tokio::{sync::Mutex, task::JoinSet};
use volume_tracker::{
    platform_init, Device, FileSystem, NotificationSource, PlatformNotifier, SpawnerDisposition,
};

#[derive(Debug, Parser)]
struct Cli {
    #[clap(short, long, default_value = "config.yaml")]
    config: PathBuf,
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    let args = Cli::parse();

    let config: Config = serde_yaml::from_reader(std::fs::File::open(args.config).unwrap())
        .expect("Failed to read config file");

    if let Err(e) = config.validate() {
        log::error!("Invalid config: {}", e);
        std::process::exit(1);
    }
    if config.pairs.is_empty() {
        log::warn!("No sync pairs set up, demonstrating only");
    }

    platform_init().expect("Failed to initialize platform");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle();
    let js = Mutex::new(JoinSet::new());

    let mp = MultiProgress::new();

    let mut s = PlatformNotifier::new(|v, d, p| match p {
        None => {
            log::info!("Device not mounted (yet): {}, {}", v.name(), d.name());
            return SpawnerDisposition::Skip;
        }
        Some(p) => {
            log::info!(
                "New device: volume: {}, device: {}, mounted at: {}",
                v.name(),
                d.name(),
                p.display()
            );
            let pairs = config
                .pairs
                .iter()
                .filter(|pair| pair.src.r#match.matches(v.name(), d.name()))
                .cloned()
                .collect::<Vec<_>>();
            if pairs.is_empty() {
                log::info!("No pairs for volume: {}, device: {}", v.name(), d.name());
                return SpawnerDisposition::Ignore;
            }

            let v_name = v.name().to_string();
            let mp = mp.clone();
            let mp2 = mp.clone();
            let pg = ProgressBar::new(0);
            let pg2 = pg.clone();
            let done = Arc::new(AtomicBool::new(false));
            let done2 = Arc::clone(&done);
            let ah = js.blocking_lock().spawn_on(
                async move {
                    pg.set_style(
                        indicatif::ProgressStyle::default_bar()
                            .template("{msg} - [{bar:40.cyan/blue}] {pos}/{len} files")
                            .unwrap()
                            .progress_chars("=> "),
                    );
                    mp.add(pg.clone());
                    for pair in pairs {
                        pg.set_message(format!(
                            "(Discovery in progress) {}",
                            pair.src.path.display()
                        ));
                        SyncFS::new(&pair.src.path, &pair.dest.path, pair.concurrency)
                            .sync(
                                |gp, ms| {
                                    if let Some(ProgressMilestone::DiscoveryComplete) = ms {
                                        pg.set_message(pair.src.path.display().to_string());
                                    }
                                    pg.set_length(gp.files.total.load(Ordering::Relaxed));
                                    pg.set_position(gp.files.done.load(Ordering::Relaxed));
                                },
                                &|e| {
                                    if let Err(e) = mp.println(format!(
                                        "Error syncing {}: {}",
                                        pair.src.path.display(),
                                        e
                                    )) {
                                        log::error!("Failed to print sync error: {}", e);
                                    }
                                },
                            )
                            .await
                    }
                    pg.finish_with_message(format!("Synced {}", v.name()));
                    mp.remove(&pg);
                    done.store(true, Ordering::SeqCst);
                },
                handle,
            );
            SpawnerDisposition::Spawned(
                ah,
                Some(Box::new(move || {
                    if done2.load(Ordering::SeqCst) {
                        return;
                    }
                    pg2.finish_with_message(format!("Aborted {}", v_name));
                    mp2.remove(&pg2);
                })),
            )
        }
    })
    .expect("Failed to create PlatformNotifier");

    s.list_spawn().unwrap();
    s.start().unwrap();

    log::info!("Successfully set up watcher!");

    let wait_tasks = async {
        loop {
            let res = js.lock().await.join_next().await;
            match res {
                None => {
                    break;
                }
                Some(Err(e)) => {
                    if e.is_cancelled() {
                        log::warn!("Task cancelled");
                    } else {
                        log::error!("Task failed: {:?}", e);
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    };

    rt.block_on(async {
        log::info!("Press ctrl-c to exit");
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to wait for ctrl-c");
        log::info!("Received ctrl-c, shutting down, press ctrl-c again to abort");
        s.pause().unwrap();
        tokio::select! {
            _ = wait_tasks => {
                log::info!("All tasks completed, shutting down");
            }
            _ = tokio::signal::ctrl_c() => {
                log::warn!("Received ctrl-c again, aborting");
            }
        }
    });

    log::info!("Cleaning up");
    mp.clear().unwrap();
    s.reset().unwrap();
}
