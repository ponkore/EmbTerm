//! embterm.exe: EmbTerm のデバッグ用 exe（T-027）。
//!
//! トップレベルのウィンドウ 1 枚に端末 1 つ。描画・VT・PTY は Emacs module（T-028）と同じクレート
//! （et-render・et-session（et-vt・et-pty のつなぎとキーの変換））を使う。設定はコマンドライン引数だけ（args.rs）。
//!
//! macOS では NSWindow + CAMetalLayer に Metal + CoreText で描く（T-125。window_mac.rs）。
//! `--headless` を付けると窓を作らず、`--screenshot` だけをオフスクリーンに描く（自動確認用。headless.rs）。

#![cfg_attr(windows, windows_subsystem = "windows")]

mod args;
/// ウィンドウを持たない動かし方（T-124。`--headless` と、窓を作れない OS で使う）
#[cfg(not(windows))]
mod headless;
#[cfg(windows)]
mod window;
/// macOS の窓（T-125）
#[cfg(target_os = "macos")]
mod window_mac;

use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// ログ。標準エラー（リダイレクトされていれば届く）と `--log` のファイルに書く
pub fn log(msg: &str) {
    let line = format!("[embterm {:>8.3}] {msg}", started().elapsed().as_secs_f64());
    eprintln!("{line}");
    if let Some(f) = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

fn started() -> std::time::Instant {
    static T: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *T.get_or_init(std::time::Instant::now)
}

fn main() -> std::process::ExitCode {
    started();
    // 端末を作る前に登録する（T-088）
    et_vt::install_png_decoder();
    let args = match args::parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            message_box(&e);
            return std::process::ExitCode::from(2);
        }
    };
    if let Some(p) = &args.log {
        match std::fs::File::create(p) {
            Ok(f) => *LOG_FILE.lock().unwrap() = Some(f),
            Err(e) => eprintln!("ログ {} を作れません: {e}", p.display()),
        }
    }
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("エラー: {e}"));
            message_box(&e);
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run(args: args::Args) -> Result<(), String> {
    window::run(args)
}

#[cfg(target_os = "macos")]
fn run(args: args::Args) -> Result<(), String> {
    if args.headless { headless::run(args) } else { window_mac::run(args) }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn run(args: args::Args) -> Result<(), String> {
    headless::run(args)
}

/// `--font` などからフォントの指定を作る（窓とヘッドレスで同じ）
#[cfg(not(windows))]
pub fn font_spec(args: &args::Args) -> et_render::FontSpec {
    let mut spec = et_render::FontSpec::default();
    if let Some(f) = &args.font {
        spec.families = vec![f.clone()];
    }
    if let Some(s) = args.font_size {
        spec.size_pt = s;
    }
    if !args.font_fallback.is_empty() {
        spec.fallback = args.font_fallback.clone();
    }
    spec
}

/// 起動できなかったことを伝える（Windows はメッセージボックス、ほかは標準エラーだけ）
#[cfg(windows)]
fn message_box(msg: &str) {
    window::message_box(msg);
}

#[cfg(not(windows))]
fn message_box(_msg: &str) {}
