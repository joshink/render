//! HTTP server mode (`render serve`).
//!
//! Exposes the render pipeline over HTTP as an asynchronous job API. Renders
//! are long-running, GPU-bound, blocking work, so requests never wait for a
//! render: `POST /render` validates the spec, enqueues a job, and returns a
//! job id immediately. Clients then either poll `GET /render/{id}` or
//! subscribe to `GET /render/{id}/events` for a Server-Sent Events stream of
//! status updates (queued → running w/ per-frame progress → done/failed).
//!
//! Outputs must be remote destinations (`s3://`, `gs://`, `mux://`, or a
//! signed `http(s)://` PUT URL) — the server never streams rendered bytes
//! back over the response, it reports where the output landed.
//!
//! Concurrency model: the axum/tokio side only shuffles small JSON payloads;
//! actual rendering happens on a fixed pool of OS worker threads fed by a
//! bounded queue (`--concurrency`, `--queue-capacity`). Worker panics are
//! caught and surfaced as failed jobs rather than killing the server.
//!
//! Finished (done/failed) jobs stay queryable for [`JOB_RETENTION`] and are
//! then evicted by a janitor thread; without eviction the registry would grow
//! for the life of the server. Eviction never interrupts a live SSE stream —
//! subscribers hold their own clone of the job's watch channel.
//!
//! Lock-poisoning policy: all shared mutexes are accessed via
//! `lock_unpoisoned`, which recovers a poisoned lock instead of giving up.
//! See its doc comment for why this is sound here.

use crate::config::REMOTE_SCHEMES;
use crate::pipeline::{self, Progress, RenderOptions};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use log::{error, info, warn};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How long a finished (done/failed) job's status remains queryable before
/// the janitor evicts it from the registry.
const JOB_RETENTION: Duration = Duration::from_secs(15 * 60);

/// How often the janitor sweeps for expired jobs.
const JANITOR_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

// ─── Job state ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Running,
    Done,
    Failed,
}

/// A snapshot of a job's status, broadcast to pollers and SSE subscribers.
#[derive(Clone, Debug, Serialize)]
pub struct JobState {
    pub status: JobPhase,
    /// Pipeline stage detail while `status == running`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    /// Final output location once `status == done`: the remote URI, or the
    /// Mux asset id for `mux://` destinations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl JobState {
    fn queued() -> Self {
        JobState { status: JobPhase::Queued, progress: None, output: None, error: None }
    }
    fn running(progress: Option<Progress>) -> Self {
        JobState { status: JobPhase::Running, progress, output: None, error: None }
    }
    fn done(output: String) -> Self {
        JobState { status: JobPhase::Done, progress: None, output: Some(output), error: None }
    }
    fn failed(error: String) -> Self {
        JobState { status: JobPhase::Failed, progress: None, output: None, error: Some(error) }
    }
    fn is_terminal(&self) -> bool {
        matches!(self.status, JobPhase::Done | JobPhase::Failed)
    }
}

// ─── Server plumbing ──────────────────────────────────────────────────────────

struct QueueItem {
    id: String,
    spec: crate::config::RenderSpec,
    tx: watch::Sender<JobState>,
}

type Registry = Arc<Mutex<HashMap<String, watch::Receiver<JobState>>>>;

/// Eviction schedule for finished jobs: (evict-at instant, job id), pushed by
/// workers as jobs reach a terminal state and drained by the janitor thread.
type ExpiryQueue = Arc<Mutex<VecDeque<(Instant, String)>>>;

#[derive(Clone)]
struct AppState {
    registry: Registry,
    queue: mpsc::SyncSender<QueueItem>,
}

/// Locks a shared mutex, recovering it if a panicking thread poisoned it.
///
/// Every operation performed under these locks is a single, panic-free map or
/// queue insert/remove, so a poisoned lock never guards half-mutated data —
/// the contents are still valid. The alternatives are all worse: propagating
/// the poison would permanently disable eviction (janitor), kill a worker, or
/// turn every status request into a 500 for the life of the server.
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct ServeOptions {
    pub host: String,
    pub port: u16,
    /// Number of render worker threads. Renders are GPU-bound; keep this
    /// small (1–2) unless VRAM is plentiful.
    pub concurrency: usize,
    /// Maximum number of queued (not yet running) jobs before POST /render
    /// returns 503.
    pub queue_capacity: usize,
    pub render: RenderOptions,
}

/// Starts the HTTP server and blocks until shutdown (Ctrl-C / SIGTERM).
///
/// Shutdown order: stop accepting connections and drain HTTP, then fail any
/// still-queued jobs, then join the worker threads so in-flight renders run
/// to completion (and their temp files / ffmpeg children are cleaned up)
/// before the process exits.
pub fn serve(opts: ServeOptions) -> Result<(), String> {
    let (queue_tx, queue_rx) = mpsc::sync_channel::<QueueItem>(opts.queue_capacity.max(1));
    let queue_rx = Arc::new(Mutex::new(queue_rx));
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
    let expiry: ExpiryQueue = Arc::new(Mutex::new(VecDeque::new()));
    let shutting_down = Arc::new(AtomicBool::new(false));

    let workers = opts.concurrency.max(1);
    let mut worker_handles = Vec::with_capacity(workers);
    for worker_id in 0..workers {
        let queue_rx = Arc::clone(&queue_rx);
        let render_opts = opts.render.clone();
        let expiry = Arc::clone(&expiry);
        let shutting_down = Arc::clone(&shutting_down);
        let handle = std::thread::Builder::new()
            .name(format!("render-worker-{}", worker_id))
            .spawn(move || worker_loop(worker_id, queue_rx, render_opts, expiry, shutting_down))
            .map_err(|e| format!("Failed to spawn render worker: {}", e))?;
        worker_handles.push(handle);
    }

    {
        let registry = Arc::clone(&registry);
        let expiry = Arc::clone(&expiry);
        // Detached daemon: sleeps between sweeps and dies with the process.
        std::thread::Builder::new()
            .name("job-janitor".to_string())
            .spawn(move || janitor_loop(registry, expiry))
            .map_err(|e| format!("Failed to spawn job janitor: {}", e))?;
    }

    let app = Router::new()
        .route("/render", post(submit_render))
        .route("/render/{id}", get(job_status))
        .route("/render/{id}/events", get(job_events))
        .route("/healthz", get(healthz))
        .with_state(AppState { registry, queue: queue_tx });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start tokio runtime: {}", e))?;

    let serve_result = rt.block_on(async move {
        let addr = format!("{}:{}", opts.host, opts.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind {}: {}", addr, e))?;
        info!(
            "render serve listening on http://{} ({} render worker{}, queue capacity {})",
            addr,
            workers,
            if workers == 1 { "" } else { "s" },
            opts.queue_capacity.max(1),
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| format!("Server error: {}", e))
    });

    // HTTP is drained. Workers fail (rather than render) anything still
    // queued, and dropping the runtime drops the last queue senders, which
    // ends the worker loops once the queue is empty.
    shutting_down.store(true, Ordering::SeqCst);
    drop(rt);

    info!("Waiting for in-flight renders to finish...");
    for handle in worker_handles {
        let _ = handle.join();
    }
    info!("All render workers stopped.");

    serve_result
}

/// Evicts finished jobs from the registry once their retention lapses.
fn janitor_loop(registry: Registry, expiry: ExpiryQueue) {
    loop {
        std::thread::sleep(JANITOR_SWEEP_INTERVAL);
        let now = Instant::now();
        let mut due = Vec::new();
        {
            let mut queue = lock_unpoisoned(&expiry);
            while queue.front().is_some_and(|(evict_at, _)| *evict_at <= now) {
                due.push(queue.pop_front().expect("front checked above").1);
            }
        }
        if !due.is_empty() {
            let mut registry = lock_unpoisoned(&registry);
            for id in &due {
                registry.remove(id);
            }
            info!("Evicted {} finished job(s) from the registry", due.len());
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received, stopping server...");
}

// ─── Worker loop ──────────────────────────────────────────────────────────────

fn worker_loop(
    worker_id: usize,
    queue_rx: Arc<Mutex<mpsc::Receiver<QueueItem>>>,
    render_opts: RenderOptions,
    expiry: ExpiryQueue,
    shutting_down: Arc<AtomicBool>,
) {
    loop {
        // Hold the lock only while waiting for the next item so other idle
        // workers can take subsequent jobs.
        let item = lock_unpoisoned(&queue_rx).recv();
        let Ok(QueueItem { id, spec, tx }) = item else {
            break; // queue sender dropped: server is shutting down
        };

        // Jobs still queued when shutdown begins are failed, not rendered —
        // the HTTP side is already gone, so no client could collect them.
        if shutting_down.load(Ordering::SeqCst) {
            let _ = tx.send(JobState::failed("Server shut down before the job started".to_string()));
            continue;
        }

        info!("[worker {}] starting render job {}", worker_id, id);
        let _ = tx.send(JobState::running(None));

        let progress_tx = tx.clone();
        // The pipeline still contains panicking paths (deep engine/config
        // code); a panic must fail the job, not the server.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pipeline::render(spec, &render_opts, &|p: Progress| {
                let _ = progress_tx.send(JobState::running(Some(p)));
            })
        }));

        let final_state = match result {
            Ok(Ok(outcome)) => {
                info!("[worker {}] job {} done: {}", worker_id, id, outcome.output);
                pipeline::print_performance_profile(&outcome.timings);
                JobState::done(outcome.output)
            }
            Ok(Err(e)) => {
                error!("[worker {}] job {} failed: {}", worker_id, id, e);
                JobState::failed(e)
            }
            Err(panic) => {
                let msg = panic_message(panic);
                error!("[worker {}] job {} panicked: {}", worker_id, id, msg);
                // The panic may have left this thread's cached GPU device in
                // an unusable state; force the next job to re-initialise.
                pipeline::invalidate_gpu_cache();
                JobState::failed(format!("render panicked: {}", msg))
            }
        };
        let _ = tx.send(final_state);

        // Schedule this job's registry entry for eviction once its retention
        // lapses; every queued job passes through here exactly once.
        lock_unpoisoned(&expiry).push_back((Instant::now() + JOB_RETENTION, id));
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

fn error_response(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn lookup_job(registry: &Registry, id: &str) -> Option<watch::Receiver<JobState>> {
    lock_unpoisoned(registry).get(id).cloned()
}

async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `POST /render` — accepts a render spec as JSON (or KDL with a
/// `Content-Type` containing "kdl"), validates it, and enqueues it.
/// Responds `202 Accepted` with the job id.
async fn submit_render(State(state): State<AppState>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    let src = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return error_response(StatusCode::BAD_REQUEST, "Spec body is not valid UTF-8".into()),
    };
    let spec_value = match pipeline::spec_json_from_str(src, content_type.contains("kdl")) {
        Ok(v) => v,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };

    let spec = match pipeline::finalize_spec(spec_value) {
        Ok(s) => s,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };

    // The server never returns rendered bytes; the deliverable is the remote
    // location, so local output paths are rejected outright.
    let dest = spec.output.path().to_string();
    if !spec.output.is_remote() {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "Output must be a remote destination ({}); got '{}'",
                REMOTE_SCHEMES.join(", "),
                dest
            ),
        );
    }

    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = watch::channel(JobState::queued());
    lock_unpoisoned(&state.registry).insert(id.clone(), rx);

    match state.queue.try_send(QueueItem { id: id.clone(), spec, tx }) {
        Ok(()) => {
            info!("Queued render job {} (output: {})", id, dest);
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "id": id,
                    "status": "queued",
                    "status_url": format!("/render/{}", id),
                    "events_url": format!("/render/{}/events", id),
                })),
            )
                .into_response()
        }
        Err(mpsc::TrySendError::Full(_)) => {
            lock_unpoisoned(&state.registry).remove(&id);
            warn!("Render queue full, rejecting job");
            error_response(StatusCode::SERVICE_UNAVAILABLE, "Render queue is full, retry later".into())
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            lock_unpoisoned(&state.registry).remove(&id);
            error_response(StatusCode::SERVICE_UNAVAILABLE, "Render workers are not running".into())
        }
    }
}

/// `GET /render/{id}` — current job status snapshot.
async fn job_status(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(rx) = lookup_job(&state.registry, &id) else {
        return error_response(StatusCode::NOT_FOUND, format!("Unknown job id: {}", id));
    };
    let snapshot = rx.borrow().clone();
    let mut body = serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
    body["id"] = serde_json::Value::String(id);
    Json(body).into_response()
}

/// `GET /render/{id}/events` — SSE stream of status updates. Emits the
/// current state immediately, then every change, and closes after the
/// terminal (done/failed) event. Intermediate per-frame progress updates are
/// coalesced by the watch channel if the client is slower than the renderer.
async fn job_events(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(rx) = lookup_job(&state.registry, &id) else {
        return error_response(StatusCode::NOT_FOUND, format!("Unknown job id: {}", id));
    };

    struct StreamState {
        rx: watch::Receiver<JobState>,
        first: bool,
        ended: bool,
    }

    let stream = futures_util::stream::unfold(
        StreamState { rx, first: true, ended: false },
        |mut st| async move {
            if st.ended {
                return None;
            }
            if !st.first && st.rx.changed().await.is_err() {
                // Sender dropped without a terminal state (should not happen);
                // close the stream rather than hanging the client.
                return None;
            }
            st.first = false;
            let snapshot = st.rx.borrow_and_update().clone();
            st.ended = snapshot.is_terminal();
            let event = Event::default()
                .event("status")
                .json_data(&snapshot)
                .unwrap_or_else(|_| Event::default().event("status").data("{}"));
            Some((Ok::<Event, std::convert::Infallible>(event), st))
        },
    );

    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}
