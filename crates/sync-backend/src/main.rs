use volume_tracker::{platform_init, Device, FileSystem, NotificationSource, PlatformNotifier};

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
    let handle = rt.handle().clone();

    let mut s = PlatformNotifier::new(move |mount_point, d, p| {
        let jh = handle.spawn(async move {
            log::info!(
                "New sync task: volume: {}, device: {}, mounted: {:?}",
                mount_point.name(),
                d.name(),
                p
            );
        });
        (true, Some(jh.abort_handle()))
    })
    .expect("Failed to create PlatformNotifier");

    s.list_spawn().unwrap();
    s.start().unwrap();

    log::info!("Successfully set up watcher!");

    rt.block_on(async {
        eprintln!("Press ctrl-c to exit");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("ctrl-c received, exiting");
            }
        }
    });

    log::debug!("Cleaning up");
    s.reset().unwrap();
}
