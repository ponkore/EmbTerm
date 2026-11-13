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
use et_session::keys_mac;
use et_session::{CLOSE_TIMEOUT, Colors, Event, Session, SessionOptions};
use et_vt::{KeyEncoder, RenderState};
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
    let mut encoder = KeyEncoder::new().map_err(|e| e.to_string())?;
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
            let handled = ev.r#type() == NSEventType::KeyDown && send_key(&session, &mut encoder, &ev);
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

/// キーを端末に送る（`et_session::keys_mac` に渡す。KKP とレガシーの作り分けはそちら。T-126）。送ったら true
fn send_key(session: &Session, encoder: &mut KeyEncoder, ev: &NSEvent) -> bool {
    let mods = ev.modifierFlags();
    // Command は OS のメニューに渡す
    if mods.contains(NSEventModifierFlags::Command) {
        return false;
    }
    let Some(base) = ev.charactersIgnoringModifiers() else { return false };
    let base = base.to_string();
    let Some(ch) = base.chars().next() else { return false };

    // Emacs のキーの名前に合わせる（私用領域の符号は矢印などの特殊キー）
    let name: String = match ch as u32 {
        0xF700 => "up".into(),
        0xF701 => "down".into(),
        0xF702 => "left".into(),
        0xF703 => "right".into(),
        0xF728 => "delete".into(),
        0xF729 => "home".into(),
        0xF72B => "end".into(),
        0xF72C => "prior".into(),
        0xF72D => "next".into(),
        n @ 0xF704..=0xF71B => format!("f{}", n - 0xF704 + 1),
        0x0D | 0x03 => "return".into(),
        0x09 => "tab".into(),
        0x1B => "escape".into(),
        0x7F | 0x08 => "backspace".into(),
        _ => ch.to_string(),
    };
    let mut m = String::new();
    if mods.contains(NSEventModifierFlags::Control) {
        m.push('C');
    }
    if mods.contains(NSEventModifierFlags::Option) {
        m.push('M');
    }
    // 文字は Shift を使って作られている（`A` など）ので、Shift は特殊キーのときだけ渡す
    if mods.contains(NSEventModifierFlags::Shift) && name.chars().count() > 1 {
        m.push('S');
    }
    let bytes = {
        let term = session.lock();
        keys_mac::encode_emacs_key(&term, encoder, &name, &m)
    };
    if bytes.is_empty() {
        return false;
    }
    session.write(&bytes);
    true
}
