//! 手書きの宣言を `ghostty_type_json()` と照合する（T-065）。あわせて端末と render state の基本の動きを確かめる。

use std::collections::BTreeMap;
use std::mem::{align_of, offset_of, size_of};
use std::sync::{Arc, Mutex};

use crate::ffi::*;
use crate::*;

// ---- 小さな JSON の読み取り（テストのためだけ。外部クレートを足さない） ----

#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(#[allow(dead_code)] bool),
    Num(f64),
    Str(String),
    Arr(#[allow(dead_code)] Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(m) => m.get(key).unwrap_or_else(|| panic!("JSON にキー {key} が無い")),
            _ => panic!("JSON のオブジェクトではない（{key} を引いた）"),
        }
    }
    fn num(&self) -> usize {
        match self {
            Json::Num(n) => *n as usize,
            _ => panic!("数ではない: {self:?}"),
        }
    }
    fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("文字列ではない: {self:?}"),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) {
        self.ws();
        assert_eq!(self.s[self.i], c, "JSON の {} 文字目", self.i);
        self.i += 1;
    }
    fn value(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return Json::Obj(m);
                }
                loop {
                    self.ws();
                    let k = match self.value() {
                        Json::Str(s) => s,
                        v => panic!("キーが文字列でない: {v:?}"),
                    };
                    self.eat(b':');
                    let v = self.value();
                    m.insert(k, v);
                    self.ws();
                    let c = self.s[self.i];
                    self.i += 1;
                    if c == b'}' {
                        return Json::Obj(m);
                    }
                    assert_eq!(c, b',');
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return Json::Arr(v);
                }
                loop {
                    v.push(self.value());
                    self.ws();
                    let c = self.s[self.i];
                    self.i += 1;
                    if c == b']' {
                        return Json::Arr(v);
                    }
                    assert_eq!(c, b',');
                }
            }
            b'"' => {
                self.i += 1;
                let mut out = Vec::new();
                while self.s[self.i] != b'"' {
                    if self.s[self.i] == b'\\' {
                        self.i += 1;
                        match self.s[self.i] {
                            b'n' => out.push(b'\n'),
                            b't' => out.push(b'\t'),
                            b'u' => {
                                // 型の名前には現れないので、そのまま ? にする
                                self.i += 4;
                                out.push(b'?');
                            }
                            c => out.push(c),
                        }
                    } else {
                        out.push(self.s[self.i]);
                    }
                    self.i += 1;
                }
                self.i += 1;
                Json::Str(String::from_utf8_lossy(&out).into_owned())
            }
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let st = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                Json::Num(std::str::from_utf8(&self.s[st..self.i]).unwrap().parse().unwrap())
            }
        }
    }
}

fn types() -> Json {
    let json = type_json();
    assert!(!json.is_empty());
    let root = Parser { s: json.as_bytes(), i: 0 }.value();
    assert_eq!(root.get("abi").get("os").str(), "windows");
    root.get("types").clone()
}

/// 構造体の大きさ・揃え・フィールドの位置を照合する
macro_rules! check_struct {
    ($types:expr, $name:literal, $ty:ty, [$($field:ident),* $(,)?]) => {{
        let t = $types.get($name);
        assert_eq!(t.get("size").num(), size_of::<$ty>(), "{} の大きさ", $name);
        assert_eq!(t.get("align").num(), align_of::<$ty>(), "{} の揃え", $name);
        let fields = t.get("fields");
        $(
            assert_eq!(
                fields.get(stringify!($field)).get("offset").num(),
                offset_of!($ty, $field),
                "{}.{} の位置", $name, stringify!($field)
            );
        )*
        if let Json::Obj(m) = fields {
            assert_eq!(m.len(), [$(stringify!($field)),*].len(), "{} のフィールドの数（ヘッダが増えた？）", $name);
        }
    }};
}

/// 列挙の値を照合する（`prefix` を除いた名前で引く）
fn check_enum(types: &Json, name: &str, values: &[(&str, i32)]) {
    let v = types.get(name).get("values");
    for (k, expected) in values {
        assert_eq!(v.get(k).num() as i32, *expected, "{name}::{k}");
    }
}

#[test]
fn layout_matches_type_json() {
    let t = types();
    check_struct!(t, "GhosttyString", GhosttyString, [ptr, len]);
    check_struct!(t, "GhosttyCodepointBuffer", GhosttyCodepointBuffer, [ptr, cap, len]);
    check_struct!(t, "GhosttyColorRgb", GhosttyColorRgb, [r, g, b]);
    check_struct!(t, "GhosttyStyleColor", GhosttyStyleColor, [tag, value]);
    check_struct!(t, "GhosttyStyle", GhosttyStyle, [fg_color, bg_color, underline_color, flags, reserved]);
    check_struct!(t, "GhosttySizeReportSize", GhosttySizeReportSize, [rows, columns, cell_width, cell_height]);
    check_struct!(t, "GhosttyPointCoordinate", GhosttyPointCoordinate, [x, y]);
    check_struct!(t, "GhosttyPoint", GhosttyPoint, [tag, value]);
    check_struct!(t, "GhosttyGridRef", GhosttyGridRef, [node, x, y]);
    check_struct!(t, "GhosttySelection", GhosttySelection, [start, end, rectangle]);
    check_struct!(t, "GhosttyTerminalScrollViewport", GhosttyTerminalScrollViewport, [tag, value]);
    check_struct!(t, "GhosttyTerminalScrollbar", GhosttyTerminalScrollbar, [total, offset, len]);
    check_struct!(t, "GhosttyTerminalModeConfig", GhosttyTerminalModeConfig, [mode, value]);
    check_struct!(t, "GhosttyRenderStateRowSelection", GhosttyRenderStateRowSelection, [size, start_x, end_x]);
    check_struct!(
        t,
        "GhosttyRenderStateCursor",
        GhosttyRenderStateCursor,
        [size, viewport_has_value, viewport_x, viewport_y, wide_tail, visible, blinking, password_input, visual_style]
    );
    check_struct!(
        t,
        "GhosttyRenderStateColors",
        GhosttyRenderStateColors,
        [size, background, foreground, cursor, cursor_has_value, palette]
    );
    // 共用体は大きさと揃えだけ
    for (name, size, align) in [
        ("GhosttyPointValue", size_of::<GhosttyPointValue>(), align_of::<GhosttyPointValue>()),
        (
            "GhosttyTerminalScrollViewportValue",
            size_of::<GhosttyTerminalScrollViewportValue>(),
            align_of::<GhosttyTerminalScrollViewportValue>(),
        ),
    ] {
        assert_eq!(t.get(name).get("size").num(), size, "{name} の大きさ");
        assert_eq!(t.get(name).get("align").num(), align, "{name} の揃え");
    }
    assert_eq!(t.get("GhosttyCell").get("size").num(), size_of::<GhosttyCell>());
    assert_eq!(t.get("GhosttyMode").get("size").num(), size_of::<u16>());
}

#[test]
fn enums_match_type_json() {
    let t = types();
    check_enum(
        &t,
        "GhosttyTerminalOption",
        &[
            ("USERDATA", GHOSTTY_TERMINAL_OPT_USERDATA),
            ("WRITE_PTY", GHOSTTY_TERMINAL_OPT_WRITE_PTY),
            ("BELL", GHOSTTY_TERMINAL_OPT_BELL),
            ("TITLE_CHANGED", GHOSTTY_TERMINAL_OPT_TITLE_CHANGED),
            ("SIZE", GHOSTTY_TERMINAL_OPT_SIZE),
            ("COLOR_FOREGROUND", GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND),
            ("COLOR_BACKGROUND", GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND),
            ("COLOR_CURSOR", GHOSTTY_TERMINAL_OPT_COLOR_CURSOR),
            ("SELECTION", GHOSTTY_TERMINAL_OPT_SELECTION),
            ("PWD_CHANGED", GHOSTTY_TERMINAL_OPT_PWD_CHANGED),
            ("SCROLLBACK_MAX_LINES", GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES),
            ("TERMINFO_NAME", GHOSTTY_TERMINAL_OPT_TERMINFO_NAME),
            ("MODE_DEFAULT", GHOSTTY_TERMINAL_OPT_MODE_DEFAULT),
            ("MODE", GHOSTTY_TERMINAL_OPT_MODE),
            ("RESIZE_PULL_SCROLLBACK", GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK),
        ],
    );
    check_enum(
        &t,
        "GhosttyTerminalData",
        &[
            ("COLS", GHOSTTY_TERMINAL_DATA_COLS),
            ("ROWS", GHOSTTY_TERMINAL_DATA_ROWS),
            ("CURSOR_X", GHOSTTY_TERMINAL_DATA_CURSOR_X),
            ("CURSOR_Y", GHOSTTY_TERMINAL_DATA_CURSOR_Y),
            ("ACTIVE_SCREEN", GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN),
            ("SCROLLBAR", GHOSTTY_TERMINAL_DATA_SCROLLBAR),
            ("TITLE", GHOSTTY_TERMINAL_DATA_TITLE),
            ("PWD", GHOSTTY_TERMINAL_DATA_PWD),
            ("VIEWPORT_ACTIVE", GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE),
            ("MODE", GHOSTTY_TERMINAL_DATA_MODE),
        ],
    );
    check_enum(
        &t,
        "GhosttyRenderStateData",
        &[
            ("COLS", GHOSTTY_RENDER_STATE_DATA_COLS),
            ("ROWS", GHOSTTY_RENDER_STATE_DATA_ROWS),
            ("DIRTY", GHOSTTY_RENDER_STATE_DATA_DIRTY),
            ("ROW_ITERATOR", GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR),
            ("CURSOR", GHOSTTY_RENDER_STATE_DATA_CURSOR),
            ("COLORS", GHOSTTY_RENDER_STATE_DATA_COLORS),
        ],
    );
    check_enum(
        &t,
        "GhosttyRenderStateDirty",
        &[
            ("FALSE", GHOSTTY_RENDER_STATE_DIRTY_FALSE),
            ("PARTIAL", GHOSTTY_RENDER_STATE_DIRTY_PARTIAL),
            ("FULL", GHOSTTY_RENDER_STATE_DIRTY_FULL),
        ],
    );
    check_enum(
        &t,
        "GhosttyRenderStateRowData",
        &[
            ("DIRTY", GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY),
            ("CELLS", GHOSTTY_RENDER_STATE_ROW_DATA_CELLS),
            ("SELECTION", GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION),
        ],
    );
    check_enum(
        &t,
        "GhosttyRenderStateRowCellsData",
        &[
            ("RAW", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW),
            ("STYLE", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE),
            ("GRAPHEMES_LEN", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN),
            ("GRAPHEMES_BUF", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF),
            ("BG_COLOR", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR),
            ("FG_COLOR", GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR),
        ],
    );
    check_enum(&t, "GhosttyCellData", &[("WIDE", GHOSTTY_CELL_DATA_WIDE)]);
    check_enum(
        &t,
        "GhosttyCellWide",
        &[
            ("NARROW", GHOSTTY_CELL_WIDE_NARROW),
            ("WIDE", GHOSTTY_CELL_WIDE_WIDE),
            ("SPACER_TAIL", GHOSTTY_CELL_WIDE_SPACER_TAIL),
            ("SPACER_HEAD", GHOSTTY_CELL_WIDE_SPACER_HEAD),
        ],
    );
    check_enum(
        &t,
        "GhosttyCursorStyle",
        &[
            ("BAR", GHOSTTY_CURSOR_STYLE_BAR),
            ("BLOCK", GHOSTTY_CURSOR_STYLE_BLOCK),
            ("UNDERLINE", GHOSTTY_CURSOR_STYLE_UNDERLINE),
            ("BLOCK_HOLLOW", GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW),
        ],
    );
    check_enum(
        &t,
        "GhosttyTerminalScrollViewportTag",
        &[
            ("TOP", GHOSTTY_TERMINAL_SCROLL_VIEWPORT_TOP),
            ("BOTTOM", GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM),
            ("DELTA", GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA),
        ],
    );
    check_enum(&t, "GhosttyPointTag", &[("VIEWPORT", GHOSTTY_POINT_TAG_VIEWPORT)]);
    check_enum(&t, "GhosttyKeyAction", &[("PRESS", GHOSTTY_KEY_ACTION_PRESS)]);
    check_enum(
        &t,
        "GhosttyKey",
        &[
            ("BACKSPACE", GHOSTTY_KEY_BACKSPACE),
            ("ENTER", GHOSTTY_KEY_ENTER),
            ("TAB", GHOSTTY_KEY_TAB),
            ("DELETE", GHOSTTY_KEY_DELETE),
            ("END", GHOSTTY_KEY_END),
            ("HOME", GHOSTTY_KEY_HOME),
            ("INSERT", GHOSTTY_KEY_INSERT),
            ("PAGE_DOWN", GHOSTTY_KEY_PAGE_DOWN),
            ("PAGE_UP", GHOSTTY_KEY_PAGE_UP),
            ("ARROW_DOWN", GHOSTTY_KEY_ARROW_DOWN),
            ("ARROW_LEFT", GHOSTTY_KEY_ARROW_LEFT),
            ("ARROW_RIGHT", GHOSTTY_KEY_ARROW_RIGHT),
            ("ARROW_UP", GHOSTTY_KEY_ARROW_UP),
            ("ESCAPE", GHOSTTY_KEY_ESCAPE),
            ("F1", GHOSTTY_KEY_F1),
            ("F12", GHOSTTY_KEY_F1 + 11),
        ],
    );
}

// ---- 動きの確認 ----

#[derive(Default)]
struct Log {
    pty: Vec<u8>,
    titles: Vec<String>,
    bells: usize,
}

struct Recorder(Arc<Mutex<Log>>);

impl Effects for Recorder {
    fn write_pty(&mut self, data: &[u8]) {
        self.0.lock().unwrap().pty.extend_from_slice(data);
    }
    fn bell(&mut self) {
        self.0.lock().unwrap().bells += 1;
    }
    fn title_changed(&mut self, title: &str) {
        self.0.lock().unwrap().titles.push(title.to_string());
    }
}

fn terminal(cols: u16, rows: u16) -> (Terminal, Arc<Mutex<Log>>) {
    let log = Arc::new(Mutex::new(Log::default()));
    let t = Terminal::new(cols, rows, Box::new(Recorder(log.clone()))).unwrap();
    (t, log)
}

#[test]
fn eaw_is_one_column() {
    // T-012: eaw（①★※─）は 1 桁
    for c in ['①', '★', '※', '─'] {
        assert_eq!(codepoint_width(c), 1, "{c}");
    }
    assert_eq!(codepoint_width('あ'), 2);
    let (mut t, _) = terminal(40, 5);
    t.vt_write("①あ★".as_bytes());
    assert_eq!(t.cursor(), (4, 0));
}

#[test]
fn callbacks_reply_and_title() {
    let (mut t, log) = terminal(80, 24);
    t.resize(80, 24, 9, 18).unwrap();
    // DA1、CSI 16 t（セルの画素）、OSC 2、BEL
    t.vt_write(b"\x1b[c\x1b[16t\x1b]2;hello\x07\x07");
    let log = log.lock().unwrap();
    let reply = String::from_utf8_lossy(&log.pty).into_owned();
    assert!(reply.contains("\x1b[?62;22c"), "DA1 の答え: {reply:?}");
    assert!(reply.contains("\x1b[6;18;9t"), "CSI 16 t の答え: {reply:?}");
    assert_eq!(log.titles, vec!["hello".to_string()]);
    assert_eq!(log.bells, 1);
}

#[test]
fn render_state_cells() {
    let (mut t, _) = terminal(20, 3);
    t.vt_write("a\x1b[1;31mB\x1b[0mあ\r\n\x1b[7mx".as_bytes());
    t.set_selection(Some(((0, 1), (3, 1)))).unwrap();
    let mut rs = RenderState::new().unwrap();
    rs.update(&t).unwrap();
    assert_eq!(rs.size(), (20, 3));
    assert_ne!(rs.dirty(), Dirty::Clean);
    let palette = rs.colors().palette;
    let cursor = rs.cursor();
    assert_eq!(cursor.pos, Some((1, 1)));

    let mut rows = rs.dirty_rows();
    let mut seen = Vec::new();
    let mut text = Vec::new();
    while let Some(y) = rows.next_dirty() {
        let sel = rows.selection();
        let mut cells = rows.cells();
        let mut x = 0;
        while cells.next_cell() {
            cells.graphemes(&mut text);
            if y == 0 && x == 1 {
                let s = cells.style();
                assert!(s.bold());
                assert_eq!(cells.fg(), Some(palette[1]));
            }
            if y == 0 && x == 2 {
                assert_eq!(text, vec!['あ']);
                assert_eq!(cells.wide(), Wide::Wide);
            }
            if y == 0 && x == 3 {
                assert_eq!(cells.wide(), Wide::SpacerTail);
            }
            if y == 1 && x == 0 {
                assert!(cells.style().inverse());
            }
            x += 1;
        }
        assert_eq!(x, 20);
        seen.push((y, sel));
    }
    assert_eq!(seen, vec![(0, None), (1, Some((0, 3))), (2, None)]);
    rs.clean();
    assert_eq!(rs.dirty(), Dirty::Clean);
}

#[test]
fn scroll_viewport_and_keys() {
    let (mut t, _) = terminal(10, 3);
    for i in 0..20 {
        t.vt_write(format!("line{i}\r\n").as_bytes());
    }
    assert!(t.viewport_at_bottom());
    t.scroll_viewport(Scroll::Delta(-5));
    assert!(!t.viewport_at_bottom());
    t.scroll_viewport(Scroll::Bottom);
    assert!(t.viewport_at_bottom());

    let mut enc = KeyEncoder::new().unwrap();
    assert_eq!(enc.encode(&t, Key::Up, Mods::default()), b"\x1b[A");
    assert_eq!(enc.encode(&t, Key::Enter, Mods::default()), b"\r");
    assert_eq!(enc.encode(&t, Key::Backspace, Mods::default()), b"\x7f");
    assert_eq!(enc.encode(&t, Key::F(1), Mods::default()), b"\x1bOP");
    assert_eq!(enc.encode(&t, Key::Right, Mods { ctrl: true, ..Default::default() }), b"\x1b[1;5C");
    // DECCKM（アプリケーションカーソルキー）
    t.vt_write(b"\x1b[?1h");
    assert_eq!(enc.encode(&t, Key::Up, Mods::default()), b"\x1bOA");
    // モード
    t.vt_write(b"\x1b[?1004h");
    assert!(t.mode(Mode::FOCUS_EVENT));
}
