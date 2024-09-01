use std::{
    error::Error,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
};

use tauri::{Emitter, Manager, State};
use tokio::{sync::Mutex, task::JoinSet};
use volume_tracker::{
    platform_init, Device, FileSystem, NotificationSource, PlatformNotifier, SpawnerDisposition,
};

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn wait_tasks<'r>(state: State<'r, TaskJS>) -> Result<(), ()> {
    loop {
        let res = state.0.lock().await.join_next().await;

        match res {
            None => {
                log::info!("Task completed");
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

    Ok(())
}

#[tauri::command]
fn send_message(
    tx: State<flume::Sender<(u64, Message)>>,
    id: State<TaskID>,
    msg: Message,
) -> TaskID {
    let this_id = id.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    tx.send((this_id, msg)).expect("Failed to send message");

    TaskID(AtomicU64::new(this_id))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct TaskID(AtomicU64);

pub struct TaskJS(Arc<Mutex<JoinSet<()>>>);

pub enum LazyCell<R, F: FnOnce() -> R> {
    UnEvaluated(Option<F>),
    Evaluated(R),
}

impl<R, F: FnOnce() -> R> LazyCell<R, F> {
    pub fn new(f: F) -> Self {
        LazyCell::UnEvaluated(Some(f))
    }

    pub fn get(&mut self) -> &R {
        match self {
            LazyCell::UnEvaluated(f) => {
                let r = f.take().unwrap()();
                *self = LazyCell::Evaluated(r);
                match self {
                    LazyCell::Evaluated(r) => r,
                    _ => unreachable!(),
                }
            }
            LazyCell::Evaluated(r) => r,
        }
    }
}

pub struct InitSpawn<E: Error + Send + Clone>(
    StdMutex<LazyCell<Result<(), E>, Box<dyn FnOnce() -> Result<(), E> + Send>>>,
);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Message {
    InitSpawn,
    ListMounts,
}

#[derive(Clone, serde::Serialize)]
pub enum MessageResult<T: Clone + serde::Serialize> {
    Ok(T),
    Err(String),
}

#[derive(Clone, serde::Serialize)]
pub struct MessageResultPayload<T: Clone + serde::Serialize> {
    id: u64,
    result: MessageResult<T>,
}

struct InternalState {
    initialized: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    platform_init().expect("Failed to initialize platform");

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let rt2 = rt.clone();
    let rt3 = rt.clone();
    let js = Arc::new(Mutex::new(JoinSet::new()));
    let js2 = js.clone();

    let mut s = PlatformNotifier::new(move |v, d, p| match p {
        None => {
            log::info!("Device not mounted (yet): {}, {}", v.name(), d.name());

            SpawnerDisposition::Skip
        }
        Some(p) => {
            log::info!(
                "New device: volume: {}, device: {}, mounted at: {}",
                v.name(),
                d.name(),
                p.display()
            );

            let ah = js
                .blocking_lock()
                .spawn_on(async move {}, Arc::clone(&rt3).handle());

            SpawnerDisposition::Spawned(ah, None)
        }
    })
    .expect("Failed to create PlatformNotifier");

    let state = InternalState {
        initialized: AtomicBool::new(false),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![greet, wait_tasks, send_message])
        .setup(move |app| {
            let (tx, rx) = flume::unbounded::<(u64, Message)>();

            if !app.manage(tx) {
                return Err("Failed to manage tx".into());
            }

            if !app.manage(TaskID(AtomicU64::new(0))) {
                return Err("Failed to manage id".into());
            }

            if !app.manage(TaskJS(js2)) {
                return Err("Failed to manage js".into());
            }

            let app = app.handle().to_owned();

            rt2.spawn(async move {
                while let Ok((id, msg)) = rx.recv_async().await {
                    match msg {
                        Message::InitSpawn => {
                            let success = state.initialized.compare_exchange(
                                false,
                                true,
                                Ordering::SeqCst,
                                Ordering::SeqCst,
                            );

                            app.emit(
                                "task_result",
                                MessageResultPayload {
                                    id,
                                    result: match success {
                                        Err(_) => {
                                            MessageResult::Err("Already initialized".to_string())
                                        }
                                        Ok(_) => match s.list_spawn().and_then(|_| s.start()) {
                                            Ok(_) => MessageResult::Ok(()),
                                            Err(e) => {
                                                log::error!("Failed to start notifier: {:?}", e);

                                                MessageResult::Err(format!("{:?}", e))
                                            }
                                        },
                                    },
                                },
                            )
                            .expect("Failed to emit task result");
                        }
                        Message::ListMounts => {
                            let mounts = s
                                .list()
                                .map_err(|e| format!("Failed to list mounts: {:?}", e))
                                .map(|mounts| {
                                    mounts
                                        .into_iter()
                                        .map(|(fs, dev, path)| {
                                            (
                                                fs.name().to_string(),
                                                dev.name().to_string(),
                                                path.map(|p| p.display().to_string()),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                });

                            app.emit(
                                "task_result",
                                MessageResultPayload {
                                    id,
                                    result: match mounts {
                                        Ok(mounts) => MessageResult::Ok(mounts),
                                        Err(e) => MessageResult::Err(e),
                                    },
                                },
                            )
                            .expect("Failed to emit task result");
                        }
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
