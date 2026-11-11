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
use et_session::CursorDefault;
use et_vt::{CursorShape, Rgb};
use term::{ColorConfig, Event, Request, ScrollAmount, SpawnOptions, Term};

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

/// "#rrggbb" を色にする（nil は `None`）
fn color(s: Option<String>) -> Result<Option<Rgb>> {
    let Some(s) = s else { return Ok(None) };
    let hex = s.strip_prefix('#').unwrap_or(&s);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(lisp_err(format!("色は \"#rrggbb\" で指定する: {s}")));
    }
    let v = u32::from_str_radix(hex, 16).map_err(lisp_err)?;
    Ok(Some(Rgb { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }))
}

fn color_string(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
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
/// FONT-FAMILY・FONT-SIZE（ポイント）・NARROW-WIDE（"overlap" か "shrink"）は nil なら既定、
/// SCROLLBACK はスクロールバックの行数の上限（nil なら 10000）。
/// ENV は Emacs の環境変数への変更の文字列のベクタ（nil なら変えない。T-109）: "NAME=VALUE" は設定、"NAME" は削除。
/// 前から順に当てる。`TERM` `COLORTERM` `TERM_PROGRAM` は端末が最後に決める。
/// 子プロセスは最初の `embterm-module-place' で起動する（そのときの大きさで）
#[defun(user_ptr)]
#[allow(clippy::too_many_arguments)]
fn create(
    argv: Vector<'_>,
    cwd: Option<String>,
    conpty_dir: Option<String>,
    font_family: Option<String>,
    font_size: Value<'_>,
    narrow_wide_: Option<String>,
    scrollback: Option<i64>,
    env: Option<Vector<'_>>,
) -> Result<RefCell<Term>> {
    let mut args = Vec::with_capacity(argv.len());
    for i in 0..argv.len() {
        args.push(std::ffi::OsString::from(argv.get::<String>(i)?));
    }
    let mut changes = Vec::new();
    if let Some(env) = env {
        for i in 0..env.len() {
            let e: String = env.get(i)?;
            changes.push(parse_env_change(&e).map_err(lisp_err)?);
        }
    }
    let opts = SpawnOptions {
        argv: args,
        cwd: cwd.map(PathBuf::from),
        env: changes,
        conpty_dir: conpty_dir.map(PathBuf::from),
        font_family,
        font_size: opt_num(font_size)?.map(|f| f as f32),
        narrow_wide: narrow_wide(narrow_wide_)?,
        scrollback_lines: scrollback.map(|n| n.max(0) as usize),
    };
    Ok(RefCell::new(term::spawn(opts).map_err(lisp_err)?))
}

/// 環境変数の変更 1 つ（"NAME=VALUE" は設定、"NAME" は削除）。Windows には "=C:=C:\x" のように
/// `=` で始まる名前もあるので、名前は 2 文字目以降の最初の `=` までとする
fn parse_env_change(e: &str) -> std::result::Result<(std::ffi::OsString, Option<std::ffi::OsString>), String> {
    let Some(first) = e.chars().next() else { return Err("環境変数の指定が空".into()) };
    if e.contains('\0') {
        return Err(format!("環境変数の指定が正しくない: {e:?}"));
    }
    let rest = &e[first.len_utf8()..];
    Ok(match rest.find('=') {
        Some(i) => {
            let n = first.len_utf8() + i;
            (e[..n].into(), Some(e[n + 1..].into()))
        }
        None => (e.into(), None),
    })
}

/// ペイン PANE（端末の中で一意の整数。Lisp が決める）を、フレーム WINDOW-ID（`(frame-parameter nil \\='window-id)'）の
/// クライアント座標 X Y に W x H（画素）で置いて見せる。`window-inside-pixel-edges' の値をそのまま渡す。
/// ペインが無ければ作り、フレームが変わったら付け替え、子 HWND が消えていれば作り直す
#[defun]
fn place(term: &RefCell<Term>, pane: i64, window_id_: String, x: i64, y: i64, w: i64, h: i64) -> Result<()> {
    let parent = window_id(&window_id_)?;
    term.borrow().request(Request::Place { pane, parent, x: x as i32, y: y as i32, w: w as i32, h: h as i32 });
    Ok(())
}

/// ペイン PANE を隠す（nil なら全部）。スクロール位置は残す
#[defun]
fn hide(term: &RefCell<Term>, pane: Option<i64>) -> Result<()> {
    term.borrow().request(Request::Hide(pane));
    Ok(())
}

/// ペイン PANE を消す（Emacs のウィンドウが無くなった）
#[defun]
fn remove(term: &RefCell<Term>, pane: i64) -> Result<()> {
    term.borrow().request(Request::Remove(pane));
    Ok(())
}

/// 選んでいるペイン PANE を伝える（nil は端末のウィンドウを選んでいない）。端末の大きさはこのペインに合わせる
#[defun]
fn select(term: &RefCell<Term>, pane: Option<i64>) -> Result<()> {
    term.borrow().request(Request::Select(pane));
    Ok(())
}

/// 文字列をそのまま端末に送る（貼り付けと同じく VT の素の文字列。キーとして送るときは `embterm-module-send-key'）
#[defun]
fn send_string(term: &RefCell<Term>, text: String) -> Result<()> {
    term.borrow().request(Request::SendString(text));
    Ok(())
}

/// キーを送る。KEY は 1 文字か、Emacs のファンクションキーの名前（"up" "return" "f5" "backtab" "kp-1" など）。
/// MODS は修飾キーの文字列（C = Ctrl、M = Meta / Alt、S = Shift）。
/// アプリの求めに応じて KKP / win32-input-mode / レガシーの VT で送る（T-069）
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

/// ペイン PANE をスクロールする。KIND は "lines"（N 行。負は上）、"pages"（N ページ）、"top"、"bottom"、
/// "wheel"（N ノッチ。子 HWND が受けたときと同じ扱い。マウスの報告が有効ならアプリへ）
#[defun]
fn scroll(term: &RefCell<Term>, pane: i64, kind: String, n: Option<i64>) -> Result<()> {
    let n = n.unwrap_or(0);
    let amount = match kind.as_str() {
        "lines" => ScrollAmount::Lines(n),
        "pages" => ScrollAmount::Pages(n),
        "top" => ScrollAmount::Top,
        "bottom" => ScrollAmount::Bottom,
        "wheel" => ScrollAmount::Wheel(n as i32),
        _ => return Err(lisp_err(format!("知らない KIND: {kind}"))),
    };
    term.borrow().request(Request::Scroll { pane, amount });
    Ok(())
}

/// スクロールバックの行数の上限を変える
#[defun]
fn set_scrollback(term: &RefCell<Term>, lines: i64) -> Result<()> {
    term.borrow().request(Request::SetScrollback(lines.max(0) as usize));
    Ok(())
}

/// 今の選択のテキスト（選択を終えたときに UI スレッドが作って置いたもの。端末をロックしない）。選択が無ければ nil
#[defun]
fn selection(term: &RefCell<Term>) -> Result<Option<String>> {
    Ok(term.borrow().shared().selection_text())
}

/// 選択を消す
#[defun]
fn clear_selection(term: &RefCell<Term>) -> Result<()> {
    term.borrow().request(Request::ClearSelection);
    Ok(())
}

/// copy モードの準備を頼む（待たない）。スクロールバックごとのテキストを UI スレッドで作り、
/// できたら `embterm-module-poll' の :events に (copy-ready)。PANE は位置を合わせるペイン（nil は選んでいるペイン）
#[defun]
fn request_copy(term: &RefCell<Term>, pane: Option<i64>) -> Result<()> {
    term.borrow().request(Request::Copy(pane));
    Ok(())
}

/// copy モードのテキストを受け取る。`(TEXT TOP POINT)`（TOP は見ていたペインの先頭の行、POINT はポイントを置く行。どちらも 0 から）。
/// まだできていなければ nil
#[defun]
fn take_copy<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    match term.borrow().shared().take_copy() {
        Some(c) => env.list((c.text, c.top, c.point)),
        None => ().into_lisp(env),
    }
}

/// 貼り付けの前の確認（端末のロックを一瞬だけ持ってモードを読む）。TEXT を確かめなしで送ってよければ t。
/// ブラケットペーストのモードでなく、かつ改行などを含むときは nil
#[defun]
fn paste_safe_p(term: &RefCell<Term>, text: String) -> Result<bool> {
    Ok(et_vt::paste_is_safe(&text)
        || match term.borrow().shared().session() {
            Some(s) => s.lock().mode(et_vt::Mode::BRACKETED_PASTE),
            None => true,
        })
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

/// 既定の色を変える（T-034）。FG・BG・CURSOR・SELECTION-BG・SELECTION-FG は "#rrggbb" か nil（module の既定）。
/// PALETTE は 16 個の "#rrggbb" か nil のベクタ（nil の色は組み込み）、または nil。
/// アプリが OSC 10 / 11 / 12 / 4 で変えた色はそのまま残る
#[defun]
fn set_colors(
    term: &RefCell<Term>,
    fg: Option<String>,
    bg: Option<String>,
    cursor: Option<String>,
    selection_bg: Option<String>,
    selection_fg: Option<String>,
    palette: Option<Vector<'_>>,
) -> Result<()> {
    let mut c = ColorConfig {
        foreground: color(fg)?,
        background: color(bg)?,
        cursor: color(cursor)?,
        selection_bg: color(selection_bg)?,
        selection_fg: color(selection_fg)?,
        palette: [None; 16],
    };
    if let Some(v) = palette {
        for i in 0..v.len().min(16) {
            c.palette[i] = color(v.get::<Option<String>>(i)?)?;
        }
    }
    term.borrow().request(Request::SetColors(c));
    Ok(())
}

/// カーソルの既定の形 STYLE（"block" / "bar" / "underline"）と点滅 BLINK（T-035）。
/// アプリが DECSCUSR で形を決めていない間はこれに従う
#[defun]
fn set_cursor(term: &RefCell<Term>, style: String, blink: Value<'_>) -> Result<()> {
    let shape = match style.as_str() {
        "block" => CursorShape::Block,
        "bar" => CursorShape::Bar,
        "underline" => CursorShape::Underline,
        _ => return Err(lisp_err(format!("block / bar / underline を指定する: {style}"))),
    };
    term.borrow().request(Request::SetCursor(CursorDefault { shape, blink: blink.is_not_nil() }));
    Ok(())
}

/// クリップボードの書き込み（OSC 52）を WRITE、読み取り（OSC 52 の `?`）を READ が non-nil なら許す（T-039）。
/// 端末を作った直後は両方とも許さない
#[defun]
fn set_clipboard_policy(term: &RefCell<Term>, write: Value<'_>, read: Value<'_>) -> Result<()> {
    term.borrow().request(Request::SetClipboardPolicy { write: write.is_not_nil(), read: read.is_not_nil() });
    Ok(())
}

/// ペイン PANE のクライアント座標 X Y（画素）のセルのハイパーリンク（OSC 8）の URI。無ければ nil
/// （UI スレッドに頼んで最大 2 秒待つ。デバッグ・自動確認用）
#[defun]
fn link_at(term: &RefCell<Term>, pane: i64, x: i64, y: i64) -> Result<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    term.borrow().request(Request::LinkAt { pane, x: x as i32, y: y as i32, reply: tx });
    Ok(rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten())
}

/// 描画とカーソルの状態の plist（UI スレッドに頼んで最大 2 秒待つ。デバッグ・自動確認用）。
/// `(:blink-timer :blink-on :blink-ticks :full-draws :partial-draws :cursor-shape :cursor-blinking :cursor-visible
/// :foreground :background :palette1 :cell :padding)`
#[defun]
fn debug_info<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    let (tx, rx) = std::sync::mpsc::channel();
    term.borrow().request(Request::DebugInfo(tx));
    let Ok(i) = rx.recv_timeout(std::time::Duration::from_secs(2)) else { return ().into_lisp(env) };
    let b = |v: bool| -> Result<Value<'e>> { v.into_lisp(env) };
    let items = [
        env.intern(":blink-timer")?,
        b(i.blink_timer)?,
        env.intern(":blink-on")?,
        b(i.blink_on)?,
        env.intern(":blink-ticks")?,
        (i.blink_ticks as i64).into_lisp(env)?,
        env.intern(":full-draws")?,
        (i.full_draws as i64).into_lisp(env)?,
        env.intern(":partial-draws")?,
        (i.partial_draws as i64).into_lisp(env)?,
        env.intern(":cursor-shape")?,
        env.intern(i.cursor_shape)?,
        env.intern(":cursor-blinking")?,
        b(i.cursor_blinking)?,
        env.intern(":cursor-visible")?,
        b(i.cursor_visible)?,
        env.intern(":foreground")?,
        color_string(i.foreground).into_lisp(env)?,
        env.intern(":background")?,
        color_string(i.background).into_lisp(env)?,
        env.intern(":palette1")?,
        color_string(i.palette1).into_lisp(env)?,
        env.intern(":cell")?,
        env.cons(i.cell.0 as i64, i.cell.1 as i64)?,
        env.intern(":padding")?,
        (i.padding as i64).into_lisp(env)?,
    ];
    env.list(&items)
}

/// 性能の測定の報告（T-054。UI スレッドに頼んで最大 2 秒待つ。デバッグ・測定用）。RESET が non-nil なら報告のあと消す。
/// 1 行に 1 項目の文字列（`名前 n=件数 mean= p50= p90= p99= max=`、単位は ms）
#[defun]
fn perf(term: &RefCell<Term>, reset: Value<'_>) -> Result<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    term.borrow().request(Request::Perf { reset: reset.is_not_nil(), reply: tx });
    Ok(rx.recv_timeout(std::time::Duration::from_secs(2)).ok())
}

/// いま生きている GPU デバイスの数（プロセス全体。T-054 / T-067 の確認用）
#[defun]
fn gpu_devices() -> Result<i64> {
    Ok(et_render::live_devices() as i64)
}

/// Emacs を終えるとき（`kill-emacs-hook'）に全部の端末を閉じる（T-076）。**最大 GRACE-MS + 300 ミリ秒待つ。**
/// アプリに CTRL_CLOSE_EVENT を送って GRACE-MS まで穏やかに終わるのを待ち、過ぎたら Job ごと（Job の外の子孫も）止める。
/// 戻り値は (時間内に閉じ終えた数 . 端末の数)
#[defun]
fn shutdown<'e>(env: &'e Env, grace_ms: i64) -> Result<Value<'e>> {
    let (done, total) = term::shutdown(std::time::Duration::from_millis(grace_ms.max(0) as u64));
    env.cons(done as i64, total as i64)
}

/// 子プロセスの木と Job の中身（デバッグ・自動確認用。T-073）。`名前(PID)` を空白区切りで並べ、Job に入っていないものに `!` が付く。
/// まだ起動していなければ nil
#[defun]
fn process_report(term: &RefCell<Term>) -> Result<Option<String>> {
    Ok(term.borrow().shared().session().map(|s| s.process_report()))
}

/// GPU デバイスを失ったことにする（デバッグ・自動確認用。T-067）。分け合うデバイスを捨て、開いている全部の端末に
/// 新しいデバイスへ載せ替えさせる
#[defun]
fn debug_lose_device() -> Result<()> {
    term::debug_lose_device();
    Ok(())
}

/// フォーカスの出入りを知らせる（FLAG が non-nil なら入った）
#[defun]
fn focus(term: &RefCell<Term>, flag: Value<'_>) -> Result<()> {
    term.borrow().request(Request::Focus(flag.is_not_nil()));
    Ok(())
}

/// ペイン PANE（nil は選んでいるペイン）の今の画面を PNG（PATH）に保存する（待たない。終わると `embterm-module-poll' の :events に (screenshot . PATH)）
#[defun]
fn screenshot(term: &RefCell<Term>, path: String, pane: Option<i64>) -> Result<()> {
    term.borrow().request(Request::Screenshot { pane, path: PathBuf::from(path) });
    Ok(())
}

/// 閉じる（待たない。子 HWND を壊し、PTY の後始末は裏で行う。ガベージコレクションで回収されたときも閉じる）
#[defun]
fn close(term: &RefCell<Term>) -> Result<()> {
    term.borrow().request(Request::Close);
    Ok(())
}

/// 端末の状態。世代が GENERATION と同じで知らせも無ければ nil。変わっていれば plist
/// `(:generation :alive :view :hwnd :panes :started :pid :conpty :font :title :pwd :exited :exit-code :closed :cols :rows :events)`。
/// :panes は `(ID HWND COLS ROWS SCROLLED)` のリスト。
/// :events は前回から溜まった知らせ `(clicked . PANE)` `(bell)` `(exited . CODE)` `(screenshot . PATH)`
/// `(selected . TEXT)` `(copy-ready)` `(error . MESSAGE)` `(clipboard . TEXT)` `(notification TITLE . BODY)`
/// `(progress STATE . PERCENT)` `(link . URI)` のリスト（STATE は `set' `remove' `error' `indeterminate' `pause'、
/// PERCENT は省かれていれば nil）
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
            Event::Clicked(id) => env.cons(env.intern("clicked")?, id)?,
            Event::Bell => env.list([env.intern("bell")?])?,
            Event::Exited(code) => env.cons(env.intern("exited")?, code.map(|c| c as i64))?,
            Event::Screenshot(p) => env.cons(env.intern("screenshot")?, p)?,
            Event::Selected(t) => env.cons(env.intern("selected")?, t)?,
            Event::CopyReady => env.list([env.intern("copy-ready")?])?,
            Event::Error(m) => env.cons(env.intern("error")?, m)?,
            Event::Clipboard(t) => env.cons(env.intern("clipboard")?, t)?,
            Event::Notification { title, body } => env.cons(env.intern("notification")?, env.cons(title, body)?)?,
            Event::Progress { state, percent } => {
                env.cons(env.intern("progress")?, env.cons(env.intern(state)?, percent.map(|p| p as i64))?)?
            }
            Event::Link(uri) => env.cons(env.intern("link")?, uri)?,
        });
    }
    let b = |v: bool| -> Result<Value<'e>> { v.into_lisp(env) };
    let mut panes = Vec::with_capacity(s.panes.len());
    for p in &s.panes {
        panes.push(env.list((p.id, p.hwnd as i64, p.cols as i64, p.rows as i64, b(p.scrolled)?))?);
    }
    let items = [
        env.intern(":generation")?,
        (s.generation as i64).into_lisp(env)?,
        env.intern(":alive")?,
        b(s.alive)?,
        env.intern(":view")?,
        b(s.view)?,
        env.intern(":hwnd")?,
        (s.hwnd as i64).into_lisp(env)?,
        env.intern(":panes")?,
        env.list(&panes)?,
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

/// カーソルの矩形 `(X Y W H)`（選んでいるペインの子 HWND の中の画素。`window-inside-pixel-edges' の左上からの位置）。
/// 見えなければ（遡っているペインでも）nil。
/// IME の変換ウィンドウの位置合わせに使う（T-010）
#[defun]
fn cursor<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    match term.borrow().shared().state().cursor {
        Some((x, y, w, h)) => env.list((x as i64, y as i64, w as i64, h as i64)),
        None => ().into_lisp(env),
    }
}

/// ペイン PANE の見えている行のテキスト（UI スレッドに頼んで最大 2 秒待つ。デバッグ・自動確認用）
#[defun]
fn pane_text(term: &RefCell<Term>, pane: i64) -> Result<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    term.borrow().request(Request::PaneText { pane, reply: tx });
    Ok(rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten())
}

/// 今のキーの送り方（"kitty" / "win32" / "legacy"）と KKP のフラグ。`(PROTOCOL . FLAGS)`。まだ起動していなければ nil（デバッグ・自動確認用）
#[defun]
fn key_protocol<'e>(env: &'e Env, term: &RefCell<Term>) -> Result<Value<'e>> {
    let t = term.borrow();
    let Some(s) = t.shared().session() else { return ().into_lisp(env) };
    let term = s.lock();
    let proto = match s.key_protocol(&term) {
        et_session::keys::KeyProtocol::Kitty => "kitty",
        et_session::keys::KeyProtocol::Win32 => "win32",
        et_session::keys::KeyProtocol::Legacy => "legacy",
    };
    let flags = term.kitty_keyboard_flags() as i64;
    drop(term);
    env.cons(proto, flags)
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

#[cfg(test)]
mod tests {
    use super::parse_env_change;

    #[test]
    fn env_change() {
        assert_eq!(parse_env_change("LANG=ja_JP.UTF-8").unwrap(), ("LANG".into(), Some("ja_JP.UTF-8".into())));
        assert_eq!(parse_env_change("EMPTY=").unwrap(), ("EMPTY".into(), Some("".into())));
        assert_eq!(parse_env_change("A=b=c").unwrap(), ("A".into(), Some("b=c".into())));
        assert_eq!(parse_env_change("CLAUDE_CONFIG_DIR").unwrap(), ("CLAUDE_CONFIG_DIR".into(), None));
        assert_eq!(parse_env_change(r"=C:=C:\x").unwrap(), ("=C:".into(), Some(r"C:\x".into())));
        assert_eq!(parse_env_change("名前=値").unwrap(), ("名前".into(), Some("値".into())));
        assert!(parse_env_change("").is_err());
    }
}
