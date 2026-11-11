//! embterm.exe: EmbTerm のデバッグ用 exe（T-027）。
//!
//! トップレベルのウィンドウ 1 枚に端末 1 つ。描画・VT・PTY は Emacs module（T-028）と同じクレート
//! （et-render・et-session（et-vt・et-pty のつなぎとキーの変換））を使う。設定はコマンドライン引数だけ（args.rs）。

#![windows_subsystem = "windows"]

mod args;
mod window;

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
    let args = match args::parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            window::message_box(&e);
            return std::process::ExitCode::from(2);
        }
    };
    if let Some(p) = &args.log {
        match std::fs::File::create(p) {
            Ok(f) => *LOG_FILE.lock().unwrap() = Some(f),
            Err(e) => eprintln!("ログ {} を作れません: {e}", p.display()),
        }
    }
    match window::run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("エラー: {e}"));
            window::message_box(&e);
            std::process::ExitCode::FAILURE
        }
    }
}
