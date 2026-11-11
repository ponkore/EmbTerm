//! トップレベルのウィンドウとメッセージの処理（UI スレッド）。
//!
//! - 読みのスレッドからの知らせは HWND 宛ての `PostMessage`（モーダルループ中も取りこぼさない。T-006）
//! - ウィンドウプロシージャは `catch_unwind` で包み、panic を `extern "system"` の外へ出さない
//! - `App` はスレッドローカルの `RefCell`。`SetWindowPos` などで再入したら `DefWindowProc` に任せ、
//!   取りこぼした `WM_SIZE` は描く直前にクライアント領域と見比べて拾う（GenPreview §1.3 #1〜#3）

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;

use et_render::{FontSpec, Frame, Renderer};
use et_vt::{KeyEncoder, RenderState, Rgb, Scroll};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::args::Args;
use crate::log;
use et_session::{Event, Session, SessionOptions, keys};

const CLASS: PCWSTR = w!("EmbTermWindow");
const WM_APP_OUTPUT: u32 = WM_APP + 1;
const WM_APP_TITLE: u32 = WM_APP + 2;
const WM_APP_EXITED: u32 = WM_APP + 3;
const TIMER_INPUT: usize = 1;
const TIMER_SCREENSHOT: usize = 2;
const TIMER_EXIT: usize = 3;
const TIMER_CONPTY_CHECK: usize = 4;
const TIMER_RESIZE: usize = 5;

const DEFAULT_FG: Rgb = Rgb { r: 0xd4, g: 0xd4, b: 0xd4 };
const DEFAULT_BG: Rgb = Rgb { r: 0x1e, g: 0x1e, b: 0x1e };

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn message_box(text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("EmbTerm"), MB_ICONERROR | MB_OK);
    }
}

struct App {
    hwnd: HWND,
    renderer: Renderer,
    session: Session,
    rs: RenderState,
    frame: Frame,
    encoder: KeyEncoder,
    grid: (u16, u16),
    cell: (u32, u32),
    focused: bool,
    /// 直前のキーをエンコーダで送ったので、続く WM_CHAR を捨てる
    skip_char: bool,
    /// サロゲートペアの前半
    high_surrogate: Option<u16>,
    wheel: i32,
    /// ドラッグでの選択の起点（セル）
    drag_anchor: Option<(u16, u16)>,
    inputs: Vec<Vec<u8>>,
    input_interval: u32,
    select: Option<((u16, u16), (u16, u16))>,
    resize: Option<(u16, u16)>,
    scroll_up: Option<u16>,
    screenshot: Option<PathBuf>,
    base_title: String,
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

/// クライアント領域が `w` x `h`（物理ピクセル）になるようにウィンドウの大きさを変える
fn set_client_size(hwnd: HWND, w: u32, h: u32) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn post(hwnd: isize, msg: u32) {
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
    }
}

/// `--conpty-dir` → exe の隣 → （開発中は）リポジトリの vendor/conpty の順に conpty.dll を探す
fn conpty_search(args: &Args) -> Vec<PathBuf> {
    if args.system_conpty {
        return Vec::new();
    }
    let mut v = et_pty::default_conpty_search(args.conpty_dir.clone());
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/conpty"));
    v
}

pub fn run(args: Args) -> Result<(), String> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_IBEAM) }.unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("EmbTerm"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            800,
            600,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|e| format!("ウィンドウを作れません: {e}"))?;

    // 描画とフォント（セルの大きさが決まる）
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let mut spec = FontSpec::default();
    if let Some(f) = &args.font {
        spec.families.insert(0, f.clone());
    }
    if let Some(s) = args.font_size {
        spec.size_pt = s;
    }
    let (w, h) = client_size(hwnd);
    let mut renderer = Renderer::new(hwnd, w, h, dpi, spec).map_err(|e| format!("描画の準備ができません: {e}"))?;
    if let Some(n) = args.narrow_wide {
        renderer.theme.narrow_wide = n;
    }
    let cell = renderer.cell_size();
    log(&format!(
        "フォント {} / セル {}x{} px / DPI {dpi}{}",
        renderer.font_family(),
        cell.0,
        cell.1,
        if renderer.is_warp() { " / WARP" } else { "" }
    ));

    // 桁数・行数に合わせてウィンドウの大きさを決める（App を置く前なので WM_SIZE は素通り）
    let grid = (args.cols.unwrap_or(100), args.rows.unwrap_or(30));
    let (cw, chh) = renderer.client_size_for(grid.0, grid.1);
    set_client_size(hwnd, cw, chh);
    let (w, h) = client_size(hwnd);
    renderer.resize(w, h).map_err(|e| e.to_string())?;
    let grid = renderer.grid_size(w, h);

    // 端末と PTY
    let hwnd_raw = hwnd.0 as isize;
    let notify = Arc::new(move |e: Event| match e {
        Event::Output => post(hwnd_raw, WM_APP_OUTPUT),
        Event::Title => post(hwnd_raw, WM_APP_TITLE),
        Event::Exited => post(hwnd_raw, WM_APP_EXITED),
        Event::Bell | Event::Pwd | Event::Closed => {}
    });
    let session = Session::start(
        SessionOptions {
            argv: args.command.clone(),
            cwd: args.cwd.clone(),
            cols: grid.0,
            rows: grid.1,
            cell_px: cell,
            conpty_search: conpty_search(&args),
            foreground: DEFAULT_FG,
            background: DEFAULT_BG,
            scrollback_lines: 10_000,
        },
        notify,
    )?;
    log(&format!(
        "起動: {:?} / {}x{} / pid {} / ConPTY: {}",
        if args.command.is_empty() { vec!["cmd.exe".into()] } else { args.command.clone() },
        grid.0,
        grid.1,
        session.pid(),
        session.conpty_kind()
    ));
    for w in session.warnings() {
        log(&format!("注意: {w}"));
    }

    let base_title = format!(
        "EmbTerm — {}",
        args.command.first().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "cmd.exe".into())
    );
    let app = App {
        hwnd,
        renderer,
        session,
        rs: RenderState::new().map_err(|e| e.to_string())?,
        frame: Frame::default(),
        encoder: KeyEncoder::new().map_err(|e| e.to_string())?,
        grid,
        cell,
        focused: false,
        skip_char: false,
        high_surrogate: None,
        wheel: 0,
        drag_anchor: None,
        inputs: args.inputs.iter().rev().cloned().collect(),
        input_interval: args.input_interval,
        select: args.select,
        resize: args.resize,
        scroll_up: args.scroll_up,
        screenshot: args.screenshot.clone(),
        base_title,
    };
    APP.with(|a| *a.borrow_mut() = Some(app));

    unsafe {
        let _ = SetWindowTextW(hwnd, &HSTRING::from(APP.with(|a| a.borrow().as_ref().map(|a| a.base_title.clone())).unwrap_or_default()));
        if !args.inputs.is_empty() {
            SetTimer(Some(hwnd), TIMER_INPUT, args.input_delay, None);
        }
        if args.screenshot.is_some() {
            let after = args.screenshot_after.unwrap_or(args.input_delay + args.input_interval * args.inputs.len() as u32 + 2000);
            SetTimer(Some(hwnd), TIMER_SCREENSHOT, after, None);
        }
        if let Some(ms) = args.exit_after {
            SetTimer(Some(hwnd), TIMER_EXIT, ms, None);
        }
        SetTimer(Some(hwnd), TIMER_CONPTY_CHECK, 3000, None);
        if args.resize.is_some() {
            SetTimer(Some(hwnd), TIMER_RESIZE, args.resize_after, None);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
    }
    with_app(|a| a.refresh(false));

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // ウィンドウを閉じたあと。PTY を閉じる（ClosePseudoConsole → EOF → Job で子を止める。時限付き）
    let app = APP.with(|a| a.borrow_mut().take());
    if let Some(app) = app {
        let pwd = app.session.pwd();
        if !pwd.is_empty() {
            log(&format!("最後のカレントディレクトリ（OSC 7 など）: {pwd}"));
        }
        // 閉じる処理は別のスレッドで進む（T-076）。exe はそれを待ってから終わる
        if !app.session.close().wait(et_session::CLOSE_TIMEOUT + std::time::Duration::from_secs(3)) {
            log("PTY の後始末が時間内に終わらなかった");
        }
        log("終了");
    }
    Ok(())
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

impl App {
    /// 端末から写して、変わっていれば描く。`force` なら変わっていなくても描く
    fn refresh(&mut self, force: bool) {
        self.sync_size();
        self.session.take_output();
        {
            let term = self.session.lock();
            if let Err(e) = self.rs.update(&term) {
                log(&format!("render state を更新できません: {e}"));
                return;
            }
        }
        let changed = self.frame.update(&mut self.rs);
        if changed || force {
            self.draw(None);
        }
    }

    fn draw(&mut self, capture: Option<&std::path::Path>) {
        if let Err(e) = self.renderer.draw(&self.frame, self.focused, capture) {
            log(&format!("描画に失敗（デバイスを作り直した）: {e}"));
        }
    }

    /// クライアント領域の大きさとスワップチェーン・端末の大きさを合わせる（取りこぼした WM_SIZE も拾う）
    fn sync_size(&mut self) {
        let (w, h) = client_size(self.hwnd);
        if w == 0 || h == 0 {
            return; // 最小化
        }
        if let Err(e) = self.renderer.resize(w, h) {
            log(&format!("スワップチェーンの大きさを変えられません: {e}"));
        }
        let grid = self.renderer.grid_size(w, h);
        let cell = self.renderer.cell_size();
        if grid != self.grid || cell != self.cell {
            self.grid = grid;
            self.cell = cell;
            self.session.resize(grid.0, grid.1, cell);
            log(&format!("リサイズ: {}x{}（セル {}x{} px）", grid.0, grid.1, cell.0, cell.1));
        }
    }

    /// 入力の前に、スクロールバックを遡っていたらいちばん下へ戻す
    fn snap_to_bottom(&mut self) {
        let mut t = self.session.lock();
        if !t.viewport_at_bottom() {
            t.scroll_viewport(Scroll::Bottom);
        }
    }

    fn send(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.snap_to_bottom();
        self.session.write(data);
    }

    fn key_down(&mut self, vk: u16, sys: bool) -> bool {
        let Some(key) = keys::special_key(vk) else { return false };
        let mods = keys::mods(sys);
        // Shift+PageUp / PageDown はスクロールバック
        if mods.shift && !mods.ctrl && matches!(key, et_vt::Key::PageUp | et_vt::Key::PageDown) {
            let page = self.grid.1 as isize - 1;
            self.session.lock().scroll_viewport(Scroll::Delta(if key == et_vt::Key::PageUp { -page } else { page }));
            self.refresh(false);
            return true;
        }
        let bytes = {
            let t = self.session.lock();
            self.encoder.encode(&t, key, mods)
        };
        self.skip_char = keys::produces_char(key);
        self.send(&bytes);
        true
    }

    fn char_input(&mut self, unit: u16, sys: bool) {
        if std::mem::take(&mut self.skip_char) {
            return;
        }
        let ch = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            let Some(hi) = self.high_surrogate.take() else { return };
            char::decode_utf16([hi, unit]).next().and_then(|r| r.ok())
        } else {
            char::from_u32(unit as u32)
        };
        let Some(ch) = ch else { return };
        let mut buf = Vec::with_capacity(5);
        if sys {
            // Alt+文字は ESC を前に付ける（レガシーの meta）
            buf.push(0x1b);
        }
        let mut tmp = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
        self.send(&buf);
    }

    fn wheel(&mut self, delta: i32) {
        self.wheel += delta;
        let notches = self.wheel / WHEEL_DELTA as i32;
        if notches != 0 {
            self.wheel -= notches * WHEEL_DELTA as i32;
            self.session.lock().scroll_viewport(Scroll::Delta(-(notches as isize) * 3));
            self.refresh(false);
        }
    }

    fn set_selection(&mut self, a: (u16, u16), b: (u16, u16)) {
        // 行優先で並べる
        let (s, e) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        if let Err(e) = self.session.lock().set_selection(Some((s, e))) {
            log(&format!("選択範囲を決められません: {e}"));
        }
        self.refresh(false);
    }

    fn mouse_cell(&self, lparam: LPARAM) -> (u16, u16) {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        self.renderer.cell_at(x, y, self.grid.0, self.grid.1)
    }

    fn update_title(&mut self) {
        let t = self.session.title();
        let mut title = if t.is_empty() { self.base_title.clone() } else { format!("{t} — EmbTerm") };
        if let Some(s) = self.session.exit_status() {
            title.push_str(&match s.code {
                Some(c) => format!(" [終了: コード {c}]"),
                None => " [終了]".to_string(),
            });
        }
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
        }
    }

    fn timer(&mut self, id: usize) {
        match id {
            TIMER_INPUT => {
                if let Some(data) = self.inputs.pop() {
                    log(&format!("入力を送る: {:?}", String::from_utf8_lossy(&data)));
                    self.send(&data);
                }
                unsafe {
                    if self.inputs.is_empty() {
                        let _ = KillTimer(Some(self.hwnd), TIMER_INPUT);
                    } else {
                        SetTimer(Some(self.hwnd), TIMER_INPUT, self.input_interval, None);
                    }
                }
            }
            TIMER_SCREENSHOT => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_SCREENSHOT);
                }
                if let Some(n) = self.scroll_up {
                    self.session.lock().scroll_viewport(Scroll::Delta(-(n as isize)));
                }
                if let Some((a, b)) = self.select {
                    self.set_selection(a, b);
                }
                self.refresh(false);
                if let Some(p) = self.screenshot.clone() {
                    self.draw(Some(&p));
                    log(&format!("画面を保存: {}", p.display()));
                    // 画面のテキストも隣に書く（自動の確認で見比べるため）
                    let mut text = String::new();
                    for y in 0..self.frame.rows {
                        text.push_str(&self.frame.row_text(y));
                        text.push('\n');
                    }
                    let _ = std::fs::write(p.with_extension("txt"), text);
                }
            }
            TIMER_EXIT => unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_EXIT);
                log("--exit-after で閉じる");
                let _ = DestroyWindow(self.hwnd);
            },
            TIMER_RESIZE => unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_RESIZE);
                if let Some((c, r)) = self.resize {
                    let (w, h) = self.renderer.client_size_for(c, r);
                    log(&format!("--resize: {c}x{r} にする"));
                    // SetWindowPos は WM_SIZE を同期で送る。App を借りているので届かないが、
                    // 次のメッセージで投げ直して描く直前の sync_size で拾う
                    set_client_size(self.hwnd, w, h);
                    post(self.hwnd.0 as isize, WM_APP_OUTPUT);
                }
            },
            TIMER_CONPTY_CHECK => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_CONPTY_CHECK);
                }
                // T-072: 再配布版の ConPTY なら起動直後に CSI c が来る。来なければ OS 同梱の conhost の見込み
                if self.session.saw_da1() {
                    log("ConPTY の確認: 起動直後に CSI c を受けた（再配布版）");
                } else {
                    log("ConPTY の確認: 起動直後に CSI c が来ない（OS 同梱の conhost の見込み。OpenConsole.exe の置き場所を確かめる）");
                }
            }
            _ => {}
        }
    }

    /// メッセージを処理する。`None` なら DefWindowProc に任せる
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_APP_OUTPUT => self.refresh(false),
            WM_APP_TITLE => self.update_title(),
            WM_APP_EXITED => {
                let s = self.session.exit_status();
                log(&format!("子プロセスが終わった: {s:?}"));
                self.refresh(false);
                self.update_title();
            }
            WM_SIZE => {
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    self.refresh(true);
                }
            }
            WM_PAINT => unsafe {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(self.hwnd, &mut ps);
                let _ = EndPaint(self.hwnd, &ps);
                self.refresh(true);
            },
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_SETFOCUS => {
                self.focused = true;
                // ConPTY は起動時に ?1004h を求めるので、フォーカスの出入りは必ず送る（T-070）
                self.session.write(b"\x1b[I");
                self.refresh(true);
            }
            WM_KILLFOCUS => {
                self.focused = false;
                self.session.write(b"\x1b[O");
                self.refresh(true);
            }
            WM_KEYDOWN => {
                if !self.key_down(wparam.0 as u16, false) {
                    return None;
                }
            }
            WM_SYSKEYDOWN => {
                // Alt+F4 などはウィンドウの操作に任せる
                if wparam.0 as u16 == windows::Win32::UI::Input::KeyboardAndMouse::VK_F4.0 || !self.key_down(wparam.0 as u16, true) {
                    return None;
                }
            }
            WM_CHAR => self.char_input(wparam.0 as u16, false),
            WM_SYSCHAR => self.char_input(wparam.0 as u16, true),
            WM_MOUSEWHEEL => self.wheel(((wparam.0 >> 16) & 0xffff) as i16 as i32),
            WM_LBUTTONDOWN => unsafe {
                let c = self.mouse_cell(lparam);
                self.drag_anchor = Some(c);
                SetCapture(self.hwnd);
                let _ = self.session.lock().set_selection(None);
                self.refresh(false);
            },
            WM_MOUSEMOVE => {
                if let Some(a) = self.drag_anchor {
                    let c = self.mouse_cell(lparam);
                    if c != a {
                        self.set_selection(a, c);
                    }
                }
            }
            WM_LBUTTONUP => unsafe {
                self.drag_anchor = None;
                let _ = ReleaseCapture();
            },
            WM_TIMER => self.timer(wparam.0),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let r = catch_unwind(AssertUnwindSafe(|| {
        if msg == WM_DESTROY {
            // App を借りている最中（--exit-after の DestroyWindow など）にも同期で届くので、App を介さずに処理する
            unsafe { PostQuitMessage(0) };
            return Some(LRESULT(0));
        }
        if msg == WM_DPICHANGED {
            // フォントを作り直してから、勧められた大きさにする（SetWindowPos は WM_SIZE を同期で送るので、
            // App を借りていない状態で呼ぶ）
            let dpi = (wparam.0 & 0xffff) as u32;
            with_app(|a| {
                if let Err(e) = a.renderer.set_dpi(dpi) {
                    log(&format!("DPI {dpi} のフォントを作れません: {e}"));
                }
                log(&format!("DPI が変わった: {dpi}"));
            });
            unsafe {
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            with_app(|a| a.refresh(true));
            return Some(LRESULT(0));
        }
        with_app(|a| a.handle(msg, wparam, lparam)).flatten()
    }));
    match r {
        Ok(Some(l)) => l,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(_) => {
            log(&format!("ウィンドウプロシージャで panic（メッセージ 0x{msg:x}）"));
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}
