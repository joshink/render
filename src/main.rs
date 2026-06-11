use log::{error, LevelFilter};
use render_poc::config::sort_value_keyframes;
use render_poc::pipeline::{self, RenderOptions};
use render_poc::serve::{serve, ServeOptions};
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

// ─── Logging ──────────────────────────────────────────────────────────────────

struct DualLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for DualLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let msg = format!("[{}] {}", record.level(), record.args());
            println!("{}", msg);
            if let Ok(mut file_guard) = self.file.lock() {
                if let Some(ref mut file) = *file_guard {
                    let _ = writeln!(file, "{}", msg);
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file_guard) = self.file.lock() {
            if let Some(ref mut file) = *file_guard {
                let _ = file.flush();
            }
        }
    }
}

static LOGGER: DualLogger = DualLogger {
    file: Mutex::new(None),
};

// ─── CLI argument parsing ─────────────────────────────────────────────────────

#[derive(clap::Parser, Debug)]
#[command(name = "render-poc", version = "0.1.0", about = "Headless GPU-accelerated video rendering engine")]
struct Opts {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the JSON render specification
    #[arg(short = 'i', long = "input")]
    input: Option<std::path::PathBuf>,

    /// Positional fallback for input path if -i/--input is not provided
    positional_input: Option<std::path::PathBuf>,

    /// Directory to output debug frames and logs
    #[arg(long = "debug")]
    debug: Option<std::path::PathBuf>,

    /// Include directories for WGSL shaders
    #[arg(short = 'I', long = "include", global = true)]
    include_paths: Vec<std::path::PathBuf>,

    /// Override the output path in the spec
    #[arg(short = 'o', long = "output")]
    output: Option<String>,

    /// Override the composition width
    #[arg(long = "width")]
    width: Option<String>,

    /// Override the composition height
    #[arg(long = "height")]
    height: Option<String>,

    /// Override the composition fps
    #[arg(long = "fps")]
    fps: Option<String>,

    /// Override the composition duration
    #[arg(long = "duration")]
    duration: Option<String>,

    /// AWS Access Key ID override
    #[arg(long = "aws-key", global = true)]
    aws_key: Option<String>,

    /// AWS Secret Access Key override
    #[arg(long = "aws-secret", global = true)]
    aws_secret: Option<String>,

    /// GCS Access Key ID override
    #[arg(long = "gcs-key", global = true)]
    gcs_key: Option<String>,

    /// GCS Secret Access Key override
    #[arg(long = "gcs-secret", global = true)]
    gcs_secret: Option<String>,

    /// Mux API token ID override (for mux:// outputs)
    #[arg(long = "mux-token-id", global = true)]
    mux_token_id: Option<String>,

    /// Mux API token secret override (for mux:// outputs)
    #[arg(long = "mux-token-secret", global = true)]
    mux_token_secret: Option<String>,

    /// Set an arbitrary override in key=value format
    #[arg(long = "set")]
    set: Vec<String>,

    /// Print the resolved spec as JSON (after KDL transpilation and overrides)
    /// and exit without rendering. Useful for inspecting what a `.kdl` compiles to.
    #[arg(long = "emit-json")]
    emit_json: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Start an HTTP server that accepts render specs and processes them as
    /// background jobs, uploading outputs to remote destinations (s3://, gs://,
    /// mux://, or signed http(s) PUT URLs). Job status is available by polling
    /// GET /render/{id} or streaming GET /render/{id}/events (SSE).
    Serve {
        /// Address to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value_t = 8080)]
        port: u16,

        /// Number of concurrent render workers (renders are GPU-bound; keep small)
        #[arg(long, default_value_t = 1)]
        concurrency: usize,

        /// Maximum number of queued jobs before new submissions get 503
        #[arg(long, default_value_t = 64)]
        queue_capacity: usize,
    },
}

// ─── Debug directory resolution ───────────────────────────────────────────────

/// Computes a unique run directory name under `debug_dir` based on the spec
/// filename, e.g. `spec_json_render_0001/`.
fn resolve_debug_run_dir(
    debug_dir: &std::path::Path,
    spec_path: &std::path::Path,
) -> std::path::PathBuf {
    let spec_file_name = spec_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spec")
        .replace('.', "_");

    let mut counter = 1;
    loop {
        let folder_name = format!("{}_render_{:04}", spec_file_name, counter);
        let path = debug_dir.join(folder_name);
        if !path.exists() {
            return path;
        }
        counter += 1;
    }
}

/// Creates the debug directory structure and wires up the file logger.
fn setup_logging(run_dir: &std::path::Path) {
    std::fs::create_dir_all(run_dir.join("frames")).expect("Failed to create debug frames dir");
    let log_file = File::create(run_dir.join("logs.txt")).expect("Failed to create logs.txt");
    if let Ok(mut file_guard) = LOGGER.file.lock() {
        *file_guard = Some(log_file);
    }
}

/// Flushes and detaches the debug log file.
fn teardown_file_logging() {
    log::logger().flush();
    if let Ok(mut file_guard) = LOGGER.file.lock() {
        *file_guard = None;
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    use clap::Parser;
    let opts = Opts::parse();

    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info));

    let render_options = RenderOptions {
        include_paths: opts.include_paths.clone(),
        aws_key: opts.aws_key.clone(),
        aws_secret: opts.aws_secret.clone(),
        gcs_key: opts.gcs_key.clone(),
        gcs_secret: opts.gcs_secret.clone(),
        mux_token_id: opts.mux_token_id.clone(),
        mux_token_secret: opts.mux_token_secret.clone(),
        debug_run_dir: None,
    };

    if let Some(Command::Serve { host, port, concurrency, queue_capacity }) = opts.command {
        if let Err(e) = serve(ServeOptions {
            host,
            port,
            concurrency,
            queue_capacity,
            render: render_options,
        }) {
            error!("{}", e);
            std::process::exit(1);
        }
        return;
    }

    run_once(opts, render_options);
}

/// One-shot CLI render (the original behavior): load the spec, render, and
/// exit non-zero on failure.
fn run_once(opts: Opts, mut render_options: RenderOptions) {
    let start_time = Instant::now();

    let spec_path = opts.input.or(opts.positional_input).unwrap_or_else(|| {
        eprintln!("Error: Missing input spec file path. Use -i/--input or pass as a positional argument.");
        std::process::exit(1);
    });

    // ── Overrides ────────────────────────────────────────────────────────
    // Collected (and validated) before any debug run directory is created so
    // an invalid --set doesn't leave an empty numbered run dir behind.
    let mut overrides = Vec::new();
    if let Some(val) = opts.output {
        overrides.push(("output".to_string(), val));
    }
    if let Some(val) = opts.width {
        overrides.push(("composition.width".to_string(), val));
    }
    if let Some(val) = opts.height {
        overrides.push(("composition.height".to_string(), val));
    }
    if let Some(val) = opts.fps {
        overrides.push(("composition.fps".to_string(), val));
    }
    if let Some(val) = opts.duration {
        overrides.push(("composition.duration".to_string(), val));
    }
    for kv in opts.set {
        if let Some(pos) = kv.find('=') {
            overrides.push((kv[..pos].to_string(), kv[pos + 1..].to_string()));
        } else {
            eprintln!("Error: Invalid format for --set, expected key=value, got: {}", kv);
            std::process::exit(1);
        }
    }

    // ── Debug setup ──────────────────────────────────────────────────────
    let run_dir_path = opts.debug.as_ref().map(|d| resolve_debug_run_dir(d, &spec_path));
    if let Some(ref run_dir) = run_dir_path {
        setup_logging(run_dir);
    }
    render_options.debug_run_dir = run_dir_path;

    // ── Spec loading ─────────────────────────────────────────────────────
    // Accepts both `.json` specs and the readable `.kdl` front-end; a `.kdl`
    // file is transpiled to the same spec JSON before anything downstream runs.
    let spec_start = Instant::now();
    let mut spec_value = pipeline::load_spec_file(&spec_path).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });

    for (path, val) in &overrides {
        if let Err(e) = pipeline::apply_override(&mut spec_value, path, val) {
            eprintln!("Error applying override ({} = {}): {}", path, val, e);
            std::process::exit(1);
        }
    }

    if opts.emit_json {
        sort_value_keyframes(&mut spec_value);
        println!("{}", serde_json::to_string_pretty(&spec_value).expect("Failed to serialize spec"));
        return;
    }

    let spec = pipeline::finalize_spec(spec_value).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });
    let spec_load_dur = spec_start.elapsed();

    // Mux is the one destination whose handle (the asset id) is only known
    // after upload; the CLI contract is to print it to stdout.
    let dest_is_mux = spec.output.is_mux();

    // ── Render ───────────────────────────────────────────────────────────
    let result = pipeline::render(spec, &render_options, &|_| {});

    match result {
        Ok(outcome) => {
            if dest_is_mux {
                println!("{}", outcome.output);
            }
            let mut timings = outcome.timings;
            timings.total = start_time.elapsed();
            timings.spec_load = Some(spec_load_dur);
            pipeline::print_performance_profile(&timings);
            teardown_file_logging();
        }
        Err(e) => {
            error!("Render failed after {:?}: {}", start_time.elapsed(), e);
            teardown_file_logging();
            std::process::exit(1);
        }
    }
}
