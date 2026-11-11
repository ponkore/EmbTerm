//! Emacs dynamic module（embterm_module.dll。T-028）。
//!
//! Lisp からは `embterm-module-*` として見える。使い方は lisp/embterm.el。
//! 端末 1 つにつき UI スレッド 1 本が、Emacs のフレームの子 HWND に端末を描く（[`term`]）。
//!
//! - **ここにある関数は待たない**（頼みごとを積んで `PostMessage` するか、共有の状態を写すだけ）。
//!   例外はデバッグ用の [`text`]（端末をロックして画面を読む）
//! - module → Lisp の知らせは [`poll`] をタイマーで呼んで読む（`open_channel` は CRT を静的リンクした module では使えない。
//!   GenPreview ADR 0006）
//! - `#[defun]` の中の panic は emacs クレートが Lisp のエラー（`rust-panic`）に変える。UI スレッドの本体・wndproc・
//!   user-ptr の `Drop` は自分で `catch_unwind` する（T-031。term.rs）
//! - **`SetProcessDpiAwareness` 系は呼ばない**（Emacs のプロセスの設定を変えてしまう）

mod term;

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use emacs::{Env, IntoLisp, Result, Value, Vector, defun};
use et_render::NarrowWide;
use term::{Event, Request, SpawnOptions, Term};

emacs::plugin_is_GPL_compatible!();

#[emacs::module(name = "embterm-module", defun_prefix = "embterm-module", separator = "-")]
fn init(_: &Env) -> Result<()> {
    Ok(())
}

static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// ログ（[`set_log`] でファイルを決めたときだけ書く）。どのスレッドから呼んでもよい
pub(crate) fn log(msg: &str) {
    let mut g = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = g.as_mut() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let _ = writeln!(f, "[{}.{:03}] {msg}", t / 1000 % 100_000, t % 1000);
    }
}

fn lisp_err(e: impl std::fmt::Display) -> emacs::Error {
    emacs::Error::msg(e.to_string())
}

/// 整数でも小数でも受ける
fn num(v: Value) -> Result<f64> {
    match v.into_rust::<f64>() {
        Ok(f) => Ok(f),
        Err(_) => Ok(v.into_rust::<i64>()? as f64),
    }
}

fn opt_num(v: Value) -> Result<Option<f64>> {
    if v.is_not_nil() { num(v).map(Some) } else { Ok(None) }
}

fn window_id(s: &str) -> Result<isize> {
    s.trim().parse::<isize>().map_err(|_| lisp_err(format!("window-id ではありません: {s}")))
}

fn narrow_wide(s: Option<String>) -> Result<NarrowWide> {
    match s {
        None => Ok(NarrowWide::default()),
        Some(s) => s.parse().map_err(lisp_err),
    }
}

/// module の版
#[defun]
fn version() -> Result<String> {
    Ok(env!("CARGO_PKG_VERSION").to_string())
}

/// ログを FILE に書く（nil ならやめる）。デバッグ用
#[defun]
fn set_log(file: Option<String>) -> Result<()> {
    let f = match file {
        Some(p) => Some(std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(lisp_err)?),
        None => None,
    };
    *LOG_FILE.lock().unwrap_or_else(|e| e.into_inner()) = f;
    log(&format!("embterm-module {} のログを始める", env!("CARGO_PKG_VERSION")));
    Ok(())
}

/// 端末を作る。ARGV はコマンドと引数の文字列のベクタ、CWD は作業ディレクトリ（nil なら Emacs と同じ）、
/// CONPTY-DIR は conpty.dll（と x64/OpenConsole.exe）のあるディレクトリ（nil か無ければ OS 同梱の ConPTY）。
/// FONT-FAMILY・FONT-SIZE（ポイント）・NARROW-WIDE（"overlap" か "shrink"）は nil なら既定。
/// 子プロセスは最初の `embterm-module-place' で起動する（そのときの大きさで）
#[defun(user_ptr)]
fn create(
    argv: Vector<'_>,
    cwd: Option<String>,
    conpty_dir: Option<String>,
    font_family: Option<String>,
    font_size: Value<'_>,
    narrow_wide_: Option<String>,
) -> Result<RefCell<Term>> {
    let mut args = Vec::with_capacity(argv.len());
    for i in 0..argv.len() {
        args.push(std::ffi::OsString::from(argv.get::<String>(i)?));
    }
    let opts = SpawnOptions {
        argv: args,
        cwd: cwd.map(PathBuf::from),
        conpty_dir: conpty_dir.map(PathBuf::from),
        font_family,
        font_size: opt_num(font_size)?.map(|f| f as f32),
        narrow_wide: narrow_wide(narrow_wide_)?,
    };
    Ok(RefCell::new(term::spawn(opts).map_err(lisp_err)?))
}

/// フレーム WINDOW-ID（`(frame-parameter nil \\='window-id)'）のクライアント座標 X Y に W x H（画素）で置いて見せる。
/// `window-inside-pixel-edges' の値をそのまま渡す。フレームが変わったら付け替え、子 HWND が消えていれば作り直す
#[defun]
fn place(term: &RefCell<Term>, window_id_: String, x: i64, y: i64, w: i64, h: i64) -> Result<()> {
    let parent = window_id(&window_id_)?;
    term.borrow().request(Request::Place { parent, x: x as i32, y: y as i32, w: w as i32, h: h as i32 });
    Ok(())
}

/// 隠す（バッファが見えなくなった）
#[defun]
fn hide(term: &RefCell<Term>) -> Result<()> {
    term.borrow().request(Request::Hide);
    Ok(())
}

/// 文字列をそのまま端末に送る
#[defun]
fn send_string(term: &RefCell<Term>, text: String) -> Result<()> {
    term.borrow().request(Request::SendString(text));
    Ok(())
}

/// キーを送る。KEY は 1 文字か、Emacs のファンクションキーの名前（"up" "return" "f5" "backtab" など）。
/// MODS は修飾キーの文字列（C = Ctrl、M = Meta / Alt、S = Shift）
#[defun]
fn send_key(term: &RefCell<Term>, key: String, mods: Option<String>) -> Result<()> {
    term.borrow().request(Request::SendKey { key, mods: mods.unwrap_or_default() });
    Ok(())
}

/// 貼り付ける（ブラケットペーストのモードなら包む。改行は CR にする）
#[defun]
fn paste(term: &RefCell<Term>, text: String) -> Result<()> {
    term.borrow().request(Request::Paste(text));
    Ok(())
}

/// スクロールバックを LINES 行動かす（負は上）。nil ならいちばん下へ
#[defun]
fn scroll(term: &RefCell<Term>, lines: Option<i64>) -> Result<()> {
    term.borrow().request(Request::Scroll(lines.map(|n| n as isize)));
    Ok(())
}

/// フォントを替える（FAMILY・SIZE が nil なら既定）
#[defun]
fn set_font(term: &RefCell<Term>, family: Option<String>, size: Value<'_>) -> Result<()> {
    let size = opt_num(size)?.map(|f| f as f32);
    term.borrow().request(Request::SetFont { family, size });
    Ok(())
}

/// 幅 1 と数えたのにグリフが広い文字の描き方（"overlap" か "shrink"。T-013）
#[defun]
fn set_narrow_wide(term: &RefCell<Term>, mode: String) -> Result<()> {
    term.borrow().request(Request::SetNarrowWide(narrow_wide(Some(mode))?));
    Ok(())
}

/// フォーカスの出入りを知らせる（FLAG が non-nil なら入った）
#[defun]
fn focus(term: &RefCell<Term>, flag: Value<'_>) -> Result<()> {
    term.borrow().request(Request::Focus(flag.is_not_nil()));
    Ok(())
}

/// 今の画面を PNG（PATH）に保存する（待たない。終わると `embterm-module-poll' の :events に (screenshot . PATH)）
#[defun]
fn screenshot(term: &RefCell<Term>, path: String) -> Result<()> {
    term.borrow().request(Request::Screenshot(PathBuf::from(path)));
    Ok(())
}

/// 閉じる（待たない。子 HWND を壊し、PTY の後始末は裏で行う。ガベージコレクションで回収されたときも閉じる）
#[defun]
fn close(term: &RefCell<Term>) -> Result<()> {
    term.borrow().request(Request::Close);
    Ok(())
}

/// 端末の状態。世代が GENERATION と同じで知らせも無ければ nil。変わっていれば plist
/// `(:generation :alive :view :hwnd :started :pid :conpty :font :title :pwd :exited :exit-code :closed :cols :rows :events)`。
/// :events は前回から溜まった知らせ `(clicked)` `(bell)` `(exited . CODE)` `(screenshot . PATH)` `(error . MESSAGE)` のリスト
#[defun]
fn poll<'e>(env: &'e Env, term: &RefCell<Term>, generation: Option<i64>) -> Result<Value<'e>> {
    let t = term.borrow();
    let s = t.shared().state();
    if generation == Some(s.generation as i64) {
        return ().into_lisp(env);
    }
    let mut events = Vec::new();
    for e in t.shared().take_events() {
        events.push(match e {
            Event::Clicked => env.list([env.intern("clicked")?])?,
            Event::Bell => env.list([env.intern("bell")?])?,
            Event::Exited(code) => env.cons(env.intern("exited")?, code.map(|c| c as i64))?,
            Event::Screenshot(p) => env.cons(env.intern("screenshot")?, p)?,
            Event::Error(m) => env.cons(env.intern("error")?, m)?,
        });
    }
    let b = |v: bool| -> Result<Value<'e>> { v.into_lisp(env) };
    let items = [
        env.intern(":generation")?,
        (s.generation as i64).into_lisp(env)?,
        env.intern(":alive")?,
        b(s.alive)?,
        env.intern(":view")?,
        b(s.view)?,
        env.intern(":hwnd")?,
        (s.hwnd as i64).into_lisp(env)?,
        env.intern(":started")?,
        b(s.started)?,
        env.intern(":pid")?,
        (s.pid as i64).into_lisp(env)?,
        env.intern(":conpty")?,
        s.conpty.into_lisp(env)?,
        env.intern(":font")?,
        s.font.into_lisp(env)?,
        env.intern(":title")?,
        s.title.into_lisp(env)?,
        env.intern(":pwd")?,
        s.pwd.into_lisp(env)?,
        env.intern(":exited")?,
        b(s.exited)?,
        env.intern(":exit-code")?,
        s.exit_code.map(|c| c as i64).into_lisp(env)?,
        env.intern(":closed")?,
        b(s.closed)?,
        env.intern(":cols")?,
        (s.cols as i64).into_lisp(env)?,
        env.intern(":rows")?,
        (s.rows as i64).into_lisp(env)?,
        env.intern(":events")?,
        env.list(&events)?,
    ];
    env.list(&items)
}

/// カーソルの矩形 `(X Y W H)`（子 HWND の中の画素。`window-inside-pixel-edges' の左上からの位置）。見えなければ nil。
/// IME の変換ウィンドウの位置合わせに使う（T-010）
#[defun]
fn cursor<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    match term.borrow().shared().state().cursor {
        Some((x, y, w, h)) => env.list((x as i64, y as i64, w as i64, h as i64)),
        None => ().into_lisp(env),
    }
}

/// 今の画面（ビューポート）のテキスト。行末の空白は落とし、行ごとに改行を付ける。まだ起動していなければ nil。
/// **端末をロックして読む**（デバッグ・自動確認用）
#[defun]
fn text<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    match term.borrow().shared().session() {
        Some(s) => term::screen_text(&s).map_err(lisp_err)?.into_lisp(env),
        None => ().into_lisp(env),
    }
}
