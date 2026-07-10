mod capture;
mod encoder;
mod h264;
mod pipeline;
mod server;
mod update_checker;
mod webrtc;

use std::io::Write;

use clap::Parser;
use screencapturekit::prelude::*;


// ── CLI argument parsing via clap derive ──

#[derive(Parser)]
#[command(
    name = "mac-screen-cast",
    version,
    about = "Stream macOS screen to browser over LAN",
    after_help = "\
EXAMPLES:
  mac-screen-cast -l --json              List windows as JSON
  mac-screen-cast -w 12345 --width 720   Stream at 720p
  mac-screen-cast -w 12345 --fps 60      Stream at 60 fps

HTTP API (at runtime):
  GET  /         HTML player page
  GET  /offer    WebRTC SDP offer
  POST /signal   WebRTC answer + ICE candidates (JSON body)
  GET  /latency  Current capture-to-send latency (ms)
"
)]
struct Cli {
    /// Window ID to capture (omit for interactive picker)
    #[arg(short = 'w', long = "window-id")]
    window_id: Option<u32>,

    /// Max output width in pixels
    #[arg(long, default_value_t = 1280)]
    width: u32,

    /// Target frame rate (1-60)
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    fps: u32,

    /// HTTP server port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// List visible windows
    #[arg(short, long)]
    list: bool,

    /// JSON output for --list
    #[arg(long)]
    json: bool,
}



// ── Capture setup: dimensions + SCContentFilter ──

struct CaptureSetup {
    out_w: u32,
    out_h: u32,
    filter: SCContentFilter,
}

fn init_capture(wid: u32, max_w: u32) -> CaptureSetup {
    let content = SCShareableContent::get().unwrap_or_else(|e| {
        eprintln!("ScreenCaptureKit error: {e}");
        eprintln!(
            "Grant Screen Recording permission in \
             System Settings > Privacy & Security > Screen Recording."
        );
        std::process::exit(1);
    });
    let windows = content.windows();
    let window = windows
        .iter()
        .find(|w| w.window_id() == wid)
        .unwrap_or_else(|| {
            eprintln!("Window {wid} not found — try --list to see available windows.");
            std::process::exit(1);
        });
    let frame = window.frame();
    let nw = frame.size().width as u32;
    let nh = frame.size().height as u32;

    let (ow, oh) = if nw > max_w {
        let rnh = (nh * max_w) / nw;
        (max_w, rnh)
    } else {
        (nw, nh)
    };

    let filter = SCContentFilter::create().with_window(window).build();

    CaptureSetup {
        out_w: ow & !1,
        out_h: oh & !1,
        filter,
    }
}

// ── Interactive window picker ──

fn select_window_interactively() -> (u32, String) {
    let wins = server::list_windows();
    if wins.is_empty() {
        eprintln!("No windows found");
        std::process::exit(1);
    }
    let mut seen = std::collections::HashSet::new();
    let uq: Vec<&server::Window> = wins
        .iter()
        .filter(|w| seen.insert((&w.app, &w.title)))
        .collect();

    for (j, w) in uq.iter().enumerate() {
        println!(
            "  [{:2}] {} - {}",
            j + 1,
            w.app,
            if w.title.len() > 55 { &w.title[..55] } else { &w.title }
        );
    }
    print!("Select window (1-{}): ", uq.len());
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();

    if let Ok(n) = s.trim().parse::<usize>() {
        if n >= 1 && n <= uq.len() {
            return (uq[n - 1].id, uq[n - 1].app.clone());
        }
    }
    std::process::exit(0);
}

// ── Print window list and exit ──

fn print_windows_and_exit(json: bool) {
    if json {
        println!("{}", server::list_windows_json());
    } else {
        for w in server::list_windows() {
            println!("{:>5} | {} | {}", w.id, w.app, w.title);
        }
    }
    std::process::exit(0);
}







fn main() {
    update_checker::check();

    let _ = rustls::crypto::CryptoProvider::install_default(
        rustls::crypto::ring::default_provider(),
    );

    let cli = Cli::parse();

    // --list: print windows and exit
    if cli.list {
        print_windows_and_exit(cli.json);
    }

    // Resolve window ID: CLI arg or interactive picker
    let (wid, window_name) = match cli.window_id {
        Some(id) if id > 0 => (id, String::from("mac-screen-cast")),
        _ => select_window_interactively(),
    };

    // ── Initialize CoreGraphics ──
    // SAFETY: sc_initialize_core_graphics() is an FFI function exported by
    // the screencapturekit crate. It initializes CoreGraphics internals and
    // must be called once before any SCStream usage in command-line tools.
    unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }

    // ── Compute output dimensions + build capture filter ──
    let CaptureSetup {
        out_w,
        out_h,
        filter,
    } = init_capture(wid, cli.width);



    // ── Run full pipeline ──
    pipeline::run_macos_pipeline(
        filter,
        pipeline::PipelineConfig {
            out_w,
            out_h,
            fps: cli.fps,
            port: cli.port,
            window_name,
        },
    );
}
