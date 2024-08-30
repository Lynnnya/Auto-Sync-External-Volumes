use std::sync::Mutex;

use tokio::task::JoinSet;
use volume_tracker::{
    platform_init, Device, FileSystem, NotificationSource, PlatformNotifier, SpawnerDisposition,
};

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    platform_init().expect("Failed to initialize platform");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle();
    let js = Mutex::new(JoinSet::new());

    let mut s = PlatformNotifier::new(|v, d, p| {
        let ah = js.lock().unwrap().spawn_on(
            async move {
                log::info!(
                    "New sync task: volume: {}, device: {}, mounted: {:?}",
                    v.name(),
                    d.name(),
                    p
                );
            },
            handle,
        );
        SpawnerDisposition::Spawned(ah)
    })
    .expect("Failed to create PlatformNotifier");

    s.list_spawn().unwrap();
    s.start().unwrap();

    log::info!("Successfully set up watcher!");

    let wait_tasks = async {
        loop {
            let res = js.lock().unwrap().join_next().await;
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
    s.reset().unwrap();
}
