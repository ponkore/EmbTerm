//! macOS の窓（T-125）。NSWindow 1 枚に端末 1 つ。
//!
//! - 本文は **layer-hosting の NSView + CAMetalLayer**。描くのは [`et_render::Renderer`]（Metal + CoreText）
//! - イベントは `nextEventMatchingMask` で自分で回す（NSApplicationDelegate もビューの派生も作らない）。
//!   キーは `sendEvent` の前に横取りして PTY に送る
//! - **キーの送り方は最小限**（文字・Enter・Tab・Esc・Backspace・矢印・Ctrl・Option = Meta）。
//!   KKP やレガシーの VT の作り分け（Windows の `et_session::keys` に当たるもの）は T-126
//! - 窓の大きさが変わったら、次の見回りでレイヤーと端末の桁数・行数を合わせる（デリゲートを作らない）

use std::time::{Duration, Instant};

use et_render::{Frame, Renderer, Theme};
use et_session::{CLOSE_TIMEOUT, Colors, Event, Session, SessionOptions};
use et_vt::RenderState;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSString};
use objc2_quartz_core::CAMetalLayer;

use crate::args::Args;
use crate::log;

/// 既定の色（Windows 版と同じ）
const DEFAULT_FG: et_vt::Rgb = et_vt::Rgb { r: 0xcc, g: 0xcc, b: 0xcc };
const DEFAULT_BG: et_vt::Rgb = et_vt::Rgb { r: 0x0c, g: 0x0c, b: 0x0c };

pub fn run(args: Args) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("main スレッドではありません")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // 1. 描く先（まずフォントの寸法を知るために、仮の大きさで作る）
    let spec = crate::font_spec(&args);
    let scale = 2.0_f32;
    let probe = Renderer::offscreen(16, 16, scale, spec.clone())?;
    let (cols, rows) = (args.cols.unwrap_or(80).max(1), args.rows.unwrap_or(24).max(1));
    let (want_w, want_h) = probe.client_size_for(cols, rows);
    let cell = probe.cell_size();
    drop(probe);

    // 2. 窓とレイヤー（大きさは論理ポイント）
    let (pw, ph) = (want_w as f64 / scale as f64, want_h as f64 / scale as f64);
    let frame_rect = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(pw, ph));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame_rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    let title = format!(
        "EmbTerm — {}",
        args.command.first().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "shell".into())
    );
    window.setTitle(&NSString::from_str(&title));

    let view = NSView::initWithFrame(NSView::alloc(mtm), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(pw, ph)));
    let layer = CAMetalLayer::new();
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));
    window.makeKeyAndOrderFront(None);
    app.activate();

    // 実際の拡大率（画面に出してから分かる）
    let scale = window.backingScaleFactor() as f32;
    let size = view_size_px(&view, scale);
    let mut renderer = Renderer::for_layer(&layer, size.0, size.1, scale, spec)?;
    let mut theme = Theme::default();
    if let Some(nw) = args.narrow_wide {
        theme.narrow_wide = nw;
    }
    renderer.set_theme(theme);
    let (cols, rows) = renderer.grid_size(size.0, size.1);
    log(&format!(
        "窓: {}x{} 画素 / 拡大率 {scale} / セル {:?} / {cols}x{rows} 桁行 / フォント {}",
        size.0,
        size.1,
        cell,
        renderer.font_family()
    ));

    // 3. 端末
    let session = Session::start(
        SessionOptions {
            argv: args.command.clone(),
            cwd: args.cwd.clone(),
            env: Vec::new(),
            cols,
            rows,
            cell_px: (renderer.cell_size().0, renderer.cell_size().1),
            conpty_search: Vec::new(),
            colors: Colors { foreground: DEFAULT_FG, background: DEFAULT_BG, cursor: None, palette: [None; 16] },
            cursor: Default::default(),
            scrollback_lines: 10_000,
            clipboard_write: false,
            clipboard_read: false,
            images: Default::default(),
        },
        std::sync::Arc::new(|_: Event| {}),
    )?;
    log(&format!("起動: pid {} / PTY: {}", session.pid(), session.pty_kind()));

    // 4. 見回しながら描く
    let mut rs = RenderState::new().map_err(|e| e.to_string())?;
    let mut frame = Frame::default();
    let start = Instant::now();
    let at = |ms: u32| start + Duration::from_millis(ms as u64);
    let mut inputs: Vec<(Instant, Vec<u8>)> = args
        .inputs
        .iter()
        .enumerate()
        .map(|(i, d)| (at(args.input_delay + args.input_interval * i as u32), d.clone()))
        .collect();
    inputs.reverse();
    let shot_at = args.screenshot.as_ref().map(|_| {
        at(args.screenshot_after.unwrap_or(args.input_delay + args.input_interval * args.inputs.len() as u32 + 2000))
    });
    let exit_at = args.exit_after.map(at);
    let mut shot_done = false;
    let mut last_size = size;

    loop {
        // イベント（キーは横取りして端末へ）
        while let Some(ev) = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                NSDefaultRunLoopMode,
                true,
            )
        } {
            let handled = ev.r#type() == NSEventType::KeyDown && send_key(&session, &ev);
            if !handled {
                app.sendEvent(&ev);
            }
        }
        if !window.isVisible() {
            log("窓が閉じられた");
            break;
        }

        // 大きさが変わっていたら合わせる
        let now_size = view_size_px(&view, window.backingScaleFactor() as f32);
        if now_size != last_size {
            last_size = now_size;
            let scale = window.backingScaleFactor() as f32;
            renderer.resize(now_size.0, now_size.1, scale)?;
            let (c, r) = renderer.grid_size(now_size.0, now_size.1);
            session.resize(c, r, renderer.cell_size());
            log(&format!("リサイズ: {}x{} 画素 / {c}x{r} 桁行", now_size.0, now_size.1));
        }

        // 自動確認の入力
        while inputs.last().is_some_and(|(t, _)| Instant::now() >= *t) {
            let (_, data) = inputs.pop().expect("直前に確かめた");
            session.write(&data);
        }

        // 端末の中身 → フレーム → 描画
        session.take_output();
        {
            let term = session.lock();
            if let Err(e) = rs.update(&term) {
                log(&format!("render state を更新できません: {e}"));
            }
            frame.capture_images(&mut rs, &term);
        }
        let changed = frame.update(&mut rs);
        let shot = shot_at.filter(|t| !shot_done && Instant::now() >= *t);
        if changed || shot.is_some() {
            let path = shot.and(args.screenshot.clone());
            if let Err(e) = renderer.draw(&frame, true, path.as_deref()) {
                log(&format!("描画に失敗: {e}"));
            }
            if let Some(p) = path {
                shot_done = true;
                let text = (0..frame.rows).map(|y| frame.row_text(y)).collect::<Vec<_>>().join("\n");
                let _ = std::fs::write(p.with_extension("txt"), text + "\n");
                log(&format!("画面を保存: {}", p.display()));
            }
        }

        if exit_at.is_some_and(|t| Instant::now() >= t) {
            log("--exit-after で閉じる");
            break;
        }
        if args.exit_on_exit && session.exit_status().is_some() {
            log("子プロセスが終わったので閉じる");
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }

    let st = renderer.stats();
    log(&format!(
        "描画: {} 回 / 画面に出した {} 回 / 見送り {} 回 / 最後の四角 {} 枚 / 1 枚 平均 {:.2} ms・最大 {:.2} ms（うち垂直同期の待ち 平均 {:.2} ms・最大 {:.2} ms）",
        st.drawn,
        st.presented,
        st.skipped,
        st.instances,
        st.total_draw.as_secs_f64() * 1000.0 / st.drawn.max(1) as f64,
        st.max_draw.as_secs_f64() * 1000.0,
        st.total_wait.as_secs_f64() * 1000.0 / st.drawn.max(1) as f64,
        st.max_wait.as_secs_f64() * 1000.0
    ));
    let closing = session.close_with(CLOSE_TIMEOUT);
    if !closing.wait(CLOSE_TIMEOUT + Duration::from_secs(2)) {
        log("閉じるのを待ちきれなかった");
    }
    Ok(())
}

/// ビューの大きさ（デバイス画素）
fn view_size_px(view: &Retained<NSView>, scale: f32) -> (u32, u32) {
    let b = view.bounds();
    (((b.size.width as f32) * scale).round().max(1.0) as u32, ((b.size.height as f32) * scale).round().max(1.0) as u32)
}

/// キーを端末に送る（**最小限**。T-126 で `et_session::keys` 相当に置き換える）。送ったら true
fn send_key(session: &Session, ev: &NSEvent) -> bool {
    let mods = ev.modifierFlags();
    // Command は OS のメニューに渡す
    if mods.contains(NSEventModifierFlags::Command) {
        return false;
    }
    let chars = ev.characters();
    let Some(chars) = chars else { return false };
    let s = chars.to_string();
    if s.is_empty() {
        return false;
    }
    let alt = mods.contains(NSEventModifierFlags::Option);
    let mut out: Vec<u8> = Vec::new();
    for ch in s.chars() {
        // 矢印・Home / End などは私用領域の符号で来る（NSUpArrowFunctionKey = 0xF700）
        let seq: Option<&[u8]> = match ch as u32 {
            0xF700 => Some(b"\x1b[A"),
            0xF701 => Some(b"\x1b[B"),
            0xF702 => Some(b"\x1b[D"),
            0xF703 => Some(b"\x1b[C"),
            0xF728 => Some(b"\x1b[3~"),
            0xF729 => Some(b"\x1b[H"),
            0xF72B => Some(b"\x1b[F"),
            0xF72C => Some(b"\x1b[5~"),
            0xF72D => Some(b"\x1b[6~"),
            _ => None,
        };
        match seq {
            Some(bytes) => out.extend_from_slice(bytes),
            None => {
                let ch = match ch {
                    '\r' | '\n' => '\r',
                    '\u{3}' => '\u{3}', // Ctrl+C はそのまま
                    c => c,
                };
                if alt {
                    out.push(0x1b); // Option を Meta として送る
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    if out.is_empty() {
        return false;
    }
    session.write(&out);
    true
}
