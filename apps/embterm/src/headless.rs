//! ウィンドウを持たない動かし方（T-124 / T-125）。
//!
//! `cargo xtask test-screens` の回帰テストはこの経路で動く（PTY → VT → 画面のテキスト）。
//! **`--screenshot` では Metal のオフスクリーンに描いて PNG も書く**（T-125）。窓を開かないので、
//! 自動確認でも描画そのものを確かめられる。
//!
//! 時刻の進み方は window.rs のタイマーと同じ:
//! `--input-delay` → `--input` を `--input-interval` おきに送る → `--screenshot-after` で撮る → `--exit-after` で閉じる

use std::time::{Duration, Instant};

use et_render::{Frame, Renderer, Theme};
use et_session::{CLOSE_TIMEOUT, Colors, Event, Session, SessionOptions};
use et_vt::RenderState;

use crate::args::Args;
use crate::log;

/// 既定の色（window.rs と同じ。テキストしか出さないので見た目には効かない）
const DEFAULT_FG: et_vt::Rgb = et_vt::Rgb { r: 0xcc, g: 0xcc, b: 0xcc };
const DEFAULT_BG: et_vt::Rgb = et_vt::Rgb { r: 0x0c, g: 0x0c, b: 0x0c };

pub fn run(args: Args) -> Result<(), String> {
    let (cols, rows) = (args.cols.unwrap_or(80).max(1), args.rows.unwrap_or(24).max(1));
    let session = Session::start(
        SessionOptions {
            argv: args.command.clone(),
            cwd: args.cwd.clone(),
            env: Vec::new(),
            cols,
            rows,
            // セルの画素（`CSI 16t` の答え。描かないので見かけの値）
            cell_px: (8, 16),
            conpty_search: Vec::new(),
            colors: Colors { foreground: DEFAULT_FG, background: DEFAULT_BG, cursor: None, palette: [None; 16] },
            cursor: Default::default(),
            scrollback_lines: 10_000,
            clipboard_write: false,
            clipboard_read: false,
            images: Default::default(),
        },
        // 知らせは使わない（画面のテキストは撮るときに端末から直に読む）
        std::sync::Arc::new(|_: Event| {}),
    )?;
    log(&format!(
        "起動（ヘッドレス）: {:?} / {cols}x{rows} / pid {} / PTY: {}",
        if args.command.is_empty() { et_pty::default_shell() } else { args.command.clone() },
        session.pid(),
        session.pty_kind()
    ));
    for w in session.warnings() {
        log(&format!("注意: {w}"));
    }

    let start = Instant::now();
    let at = |ms: u32| start + Duration::from_millis(ms as u64);
    // 入力を送る時刻
    let mut inputs: Vec<(Instant, Vec<u8>)> = args
        .inputs
        .iter()
        .enumerate()
        .map(|(i, data)| (at(args.input_delay + args.input_interval * i as u32), data.clone()))
        .collect();
    inputs.reverse(); // 後ろから取り出す
    let shot_at = args.screenshot.as_ref().map(|_| {
        at(args
            .screenshot_after
            .unwrap_or(args.input_delay + args.input_interval * args.inputs.len() as u32 + 2000))
    });
    let exit_at = args.exit_after.map(at);
    let mut shot_done = false;

    loop {
        // 入力
        while inputs.last().is_some_and(|(t, _)| Instant::now() >= *t) {
            let (_, data) = inputs.pop().expect("直前に確かめた");
            log(&format!("--input: {} バイト", data.len()));
            session.write(&data);
        }
        // 画面のテキストを撮る
        if let (Some(t), false) = (shot_at, shot_done)
            && Instant::now() >= t
        {
            shot_done = true;
            let path = args.screenshot.clone().expect("shot_at があるなら --screenshot がある");
            let text = screen_text(&session, rows);
            let txt = path.with_extension("txt");
            std::fs::write(&txt, text).map_err(|e| format!("{} に書けません: {e}", txt.display()))?;
            log(&format!("画面のテキストを保存: {}", txt.display()));
            match screenshot(&session, &args, cols, rows, &path) {
                Ok(()) => log(&format!("画面を保存: {}", path.display())),
                Err(e) => log(&format!("画面を保存できません: {e}")),
            }
        }
        // 終わり
        if exit_at.is_some_and(|t| Instant::now() >= t) {
            log("--exit-after で閉じる");
            break;
        }
        if args.exit_on_exit && session.exit_status().is_some() {
            log("子プロセスが終わったので閉じる");
            break;
        }
        if exit_at.is_none() && shot_at.is_none() && session.exit_status().is_some() {
            log("子プロセスが終わった");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    log(&format!("閉じる前のプロセス: {}", session.process_report()));
    let closing = session.close_with(CLOSE_TIMEOUT);
    if !closing.wait(CLOSE_TIMEOUT + Duration::from_secs(2)) {
        log("閉じるのを待ちきれなかった");
    }
    if let Some(r) = session.close_report() {
        log(&format!("閉じ方: {r}"));
    }
    if args.perf {
        log(&session.perf_report(false));
    }
    Ok(())
}

/// 画面（ビューポート）の行のテキスト。行末の空白はそのまま（比べる側が落とす）
fn screen_text(session: &Session, rows: u16) -> String {
    let term = session.lock();
    let mut text = term.text_rows(0, rows.saturating_sub(1) as u32, false).unwrap_or_default();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Metal のオフスクリーンに 1 枚描いて PNG にする（T-125）
fn screenshot(session: &Session, args: &Args, cols: u16, rows: u16, path: &std::path::Path) -> Result<(), String> {
    let spec = crate::font_spec(args);
    // 拡大率は 2（Retina と同じ条件で確かめる）。窓ではその画面の値を使う
    let scale = args.screenshot_scale.unwrap_or(2.0);
    let probe = Renderer::offscreen(16, 16, scale, spec.clone())?;
    let (w, h) = probe.client_size_for(cols, rows);
    drop(probe);
    let mut renderer = Renderer::offscreen(w, h, scale, spec)?;
    let mut theme = Theme::default();
    if let Some(nw) = args.narrow_wide {
        theme.narrow_wide = nw;
    }
    renderer.set_theme(theme);

    let mut rs = RenderState::new().map_err(|e| e.to_string())?;
    let mut frame = Frame::default();
    {
        let term = session.lock();
        rs.update(&term).map_err(|e| e.to_string())?;
        frame.capture_images(&mut rs, &term);
    }
    frame.update(&mut rs);
    renderer.draw(&frame, true, Some(path))
}

