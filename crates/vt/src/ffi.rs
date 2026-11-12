//! libghostty-vt 1.0 の C ABI の宣言（使う範囲だけ手で書いた。T-065）。
//!
//! 元は `vendor/ghostty-vt/include/ghostty/vt/*.h`。構造体の大きさとフィールドの位置、
//! 列挙の値は、libghostty-vt 自身が返す `ghostty_type_json()` とテスト（`tests.rs`）で照合する。
//!
//! - C の enum は `GHOSTTY_ENUM_TYPED`（= `int`）なので `c_int` で受け渡す
//! - ハンドルは不透明なポインタ
//! - 戻り値の `GhosttyResult` は 0 が成功、負が失敗

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

pub type GhosttyResult = c_int;
pub const GHOSTTY_SUCCESS: GhosttyResult = 0;
pub const GHOSTTY_OUT_OF_MEMORY: GhosttyResult = -1;
pub const GHOSTTY_INVALID_VALUE: GhosttyResult = -2;
pub const GHOSTTY_OUT_OF_SPACE: GhosttyResult = -3;
pub const GHOSTTY_NO_VALUE: GhosttyResult = -4;

// ---- 不透明なハンドル ----
pub type GhosttyTerminal = *mut c_void;
pub type GhosttyRenderState = *mut c_void;
pub type GhosttyRenderStateRowIterator = *mut c_void;
pub type GhosttyRenderStateRowCells = *mut c_void;
pub type GhosttyKeyEncoder = *mut c_void;
pub type GhosttyKeyEvent = *mut c_void;
pub type GhosttyTrackedGridRef = *mut c_void;
pub type GhosttyFormatter = *mut c_void;
pub type GhosttySelectionGesture = *mut c_void;
pub type GhosttySelectionGestureEvent = *mut c_void;
pub type GhosttyMouseEncoder = *mut c_void;
pub type GhosttyMouseEvent = *mut c_void;
pub type GhosttyRenderStateImageIterator = *mut c_void;
pub type GhosttyKittyGraphics = *mut c_void;
pub type GhosttyKittyGraphicsImage = *const c_void;
/// アロケータ。使わない（NULL を渡して既定のものにする）
pub type GhosttyAllocator = c_void;

// ---- 値の型 ----

/// types.h
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyString {
    pub ptr: *const u8,
    pub len: usize,
}

/// types.h
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyCodepointBuffer {
    pub ptr: *mut u32,
    pub cap: usize,
    pub len: usize,
}

/// color.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// style.h。`value` は共用体（パレットの番号 1 バイト、または RGB 3 バイト）なのでバイト列で持つ
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyStyleColor {
    pub tag: u8,
    pub value: [u8; 3],
}
pub const GHOSTTY_STYLE_COLOR_NONE: u8 = 0;
pub const GHOSTTY_STYLE_COLOR_PALETTE: u8 = 1;
pub const GHOSTTY_STYLE_COLOR_RGB: u8 = 2;

/// style.h（16 バイトで固定、と明記されている）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GhosttyStyle {
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub flags: u16,
    pub reserved: u16,
}
pub const GHOSTTY_STYLE_FLAG_BOLD: u16 = 1 << 0;
pub const GHOSTTY_STYLE_FLAG_ITALIC: u16 = 1 << 1;
pub const GHOSTTY_STYLE_FLAG_FAINT: u16 = 1 << 2;
pub const GHOSTTY_STYLE_FLAG_BLINK: u16 = 1 << 3;
pub const GHOSTTY_STYLE_FLAG_INVERSE: u16 = 1 << 4;
pub const GHOSTTY_STYLE_FLAG_INVISIBLE: u16 = 1 << 5;
pub const GHOSTTY_STYLE_FLAG_STRIKETHROUGH: u16 = 1 << 6;
pub const GHOSTTY_STYLE_FLAG_OVERLINE: u16 = 1 << 7;
pub const GHOSTTY_STYLE_UNDERLINE_SHIFT: u16 = 8;
pub const GHOSTTY_STYLE_UNDERLINE_MASK: u16 = 0x7 << GHOSTTY_STYLE_UNDERLINE_SHIFT;

/// size_report.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttySizeReportSize {
    pub rows: u16,
    pub columns: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// point.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyPointCoordinate {
    pub x: u16,
    pub y: u32,
}

/// point.h
#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyPointValue {
    pub coordinate: GhosttyPointCoordinate,
    pub _padding: [u64; 2],
}

/// point.h
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyPoint {
    pub tag: c_int,
    pub value: GhosttyPointValue,
}
pub const GHOSTTY_POINT_TAG_ACTIVE: c_int = 0;
pub const GHOSTTY_POINT_TAG_VIEWPORT: c_int = 1;
pub const GHOSTTY_POINT_TAG_SCREEN: c_int = 2;

/// screen.h の行（`ghostty_row_get` で読む）
pub type GhosttyRow = u64;
pub const GHOSTTY_ROW_DATA_WRAP: c_int = 1;
pub const GHOSTTY_ROW_DATA_WRAP_CONTINUATION: c_int = 2;

/// grid_ref.h
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyGridRef {
    pub node: *mut c_void,
    pub x: u16,
    pub y: u16,
}

/// selection.h
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttySelection {
    pub start: GhosttyGridRef,
    pub end: GhosttyGridRef,
    pub rectangle: bool,
}

/// terminal.h
#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyTerminalScrollViewportValue {
    pub delta: isize,
    pub row: usize,
    pub delta_prompt: isize,
    pub _padding: [u64; 2],
}

/// terminal.h（値渡しで使う）
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalScrollViewport {
    pub tag: c_int,
    pub value: GhosttyTerminalScrollViewportValue,
}
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_TOP: c_int = 0;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM: c_int = 1;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA: c_int = 2;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW: c_int = 3;

/// terminal.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyTerminalScrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

/// terminal.h。`mode` は modes.h の `GhosttyMode`（値の下 15 ビット + ANSI かどうかの最上位ビット）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyTerminalModeConfig {
    pub mode: u16,
    pub value: bool,
}

/// render.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyRenderStateOverscan {
    pub above: u16,
    pub below: u16,
}

/// render.h（大きさ付きの構造体。`size` に `size_of` を入れてから渡す）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyRenderStateRowSelection {
    pub size: usize,
    pub start_x: u16,
    pub end_x: u16,
}

/// render.h（大きさ付き）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyRenderStateCursor {
    pub size: usize,
    pub viewport_has_value: bool,
    pub viewport_x: u16,
    pub viewport_y: u16,
    pub wide_tail: bool,
    pub visible: bool,
    pub blinking: bool,
    pub password_input: bool,
    pub visual_style: c_int,
}

/// render.h（大きさ付き）
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyRenderStateColors {
    pub size: usize,
    pub background: GhosttyColorRgb,
    pub foreground: GhosttyColorRgb,
    pub cursor: GhosttyColorRgb,
    pub cursor_has_value: bool,
    pub palette: [GhosttyColorRgb; 256],
}

/// types.h（surface の座標。画素）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttySurfacePosition {
    pub x: f64,
    pub y: f64,
}

/// selection.h（ドラッグを解釈するための表示の形）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttySelectionGestureGeometry {
    pub columns: u32,
    pub cell_width: u32,
    pub padding_left: u32,
    pub screen_height: u32,
}

/// mouse/encoder.h（大きさ付き）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyMouseEncoderSize {
    pub size: usize,
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

/// mouse/event.h
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GhosttyMousePosition {
    pub x: f32,
    pub y: f32,
}

/// sys.h（PNG のデコードの結果。8 ビットの RGBA で、行の間に余白は無い）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttySysImage {
    pub width: u32,
    pub height: u32,
    pub data: *mut u8,
    pub data_len: usize,
}

/// terminal.h（クリップボードの 1 つの表現。T-039）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyClipboardContent {
    pub mime: GhosttyString,
    pub data: GhosttyString,
}

/// terminal.h（大きさ付き）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyClipboardWriteReply {
    pub size: usize,
    pub result: c_int,
    pub remember: bool,
}

/// terminal.h（書き込みの頼み。コールバックの間だけ有効）
#[repr(C)]
pub struct GhosttyClipboardWrite {
    pub location: c_int,
    pub contents: *const GhosttyClipboardContent,
    pub contents_len: usize,
    pub name: GhosttyString,
    pub granted: bool,
    pub can_remember: bool,
    pub ctx: *const c_void,
    pub reply: GhosttyClipboardWriteReplyFn,
}

/// terminal.h（大きさ付き）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyClipboardReadReply {
    pub size: usize,
    pub result: c_int,
    pub contents: *const GhosttyClipboardContent,
    pub contents_len: usize,
    pub available: *const GhosttyString,
    pub available_len: usize,
    pub remember: bool,
}

/// terminal.h（読み取りの頼み。コールバックの間だけ有効）
#[repr(C)]
pub struct GhosttyClipboardRead {
    pub location: c_int,
    pub mimes: *const GhosttyString,
    pub mimes_len: usize,
    pub list: bool,
    pub name: GhosttyString,
    pub granted: bool,
    pub can_remember: bool,
    pub ctx: *const c_void,
    pub reply: GhosttyClipboardReadReplyFn,
}

/// terminal.h（OSC 9 / 777 の通知）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyTerminalDesktopNotification {
    pub title: GhosttyString,
    pub body: GhosttyString,
}

/// terminal.h（OSC 9;4 の進捗）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GhosttyTerminalProgressReport {
    pub state: c_int,
    pub progress: i8,
}

// ---- 列挙の値（使うものだけ） ----

// GhosttyClipboardWriteResult / GhosttyClipboardReadResult（terminal.h）
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS: c_int = 0;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED: c_int = 1;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED: c_int = 2;
pub const GHOSTTY_CLIPBOARD_READ_RESULT_SUCCESS: c_int = 0;
pub const GHOSTTY_CLIPBOARD_READ_RESULT_DENIED: c_int = 1;
pub const GHOSTTY_CLIPBOARD_READ_RESULT_BUSY: c_int = 3;

// GhosttyTerminalProgressState（terminal.h）
pub const GHOSTTY_TERMINAL_PROGRESS_STATE_REMOVE: c_int = 0;
pub const GHOSTTY_TERMINAL_PROGRESS_STATE_SET: c_int = 1;
pub const GHOSTTY_TERMINAL_PROGRESS_STATE_ERROR: c_int = 2;
pub const GHOSTTY_TERMINAL_PROGRESS_STATE_INDETERMINATE: c_int = 3;
pub const GHOSTTY_TERMINAL_PROGRESS_STATE_PAUSE: c_int = 4;

// GhosttyTerminalOption
pub const GHOSTTY_TERMINAL_OPT_USERDATA: c_int = 1;
pub const GHOSTTY_TERMINAL_OPT_WRITE_PTY: c_int = 2;
pub const GHOSTTY_TERMINAL_OPT_BELL: c_int = 3;
pub const GHOSTTY_TERMINAL_OPT_XTVERSION: c_int = 5;
pub const GHOSTTY_TERMINAL_OPT_TITLE_CHANGED: c_int = 6;
pub const GHOSTTY_TERMINAL_OPT_SIZE: c_int = 7;
pub const GHOSTTY_TERMINAL_OPT_COLOR_SCHEME: c_int = 8;
pub const GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND: c_int = 12;
pub const GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND: c_int = 13;
pub const GHOSTTY_TERMINAL_OPT_COLOR_CURSOR: c_int = 14;
pub const GHOSTTY_TERMINAL_OPT_COLOR_PALETTE: c_int = 15;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT: c_int = 16;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE: c_int = 17;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE: c_int = 18;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM: c_int = 19;
pub const GHOSTTY_TERMINAL_OPT_SELECTION: c_int = 22;
pub const GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE: c_int = 23;
pub const GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK: c_int = 24;
pub const GHOSTTY_TERMINAL_OPT_PWD_CHANGED: c_int = 26;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE: c_int = 27;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES: c_int = 28;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES: c_int = 29;
pub const GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION: c_int = 30;
pub const GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT: c_int = 31;
pub const GHOSTTY_TERMINAL_OPT_TERMINFO_NAME: c_int = 38;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ: c_int = 39;
pub const GHOSTTY_TERMINAL_OPT_MODE_DEFAULT: c_int = 34;
pub const GHOSTTY_TERMINAL_OPT_MODE: c_int = 35;
pub const GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK: c_int = 41;
pub const GHOSTTY_TERMINAL_OPT_RENDER_HOLD: c_int = 42;
pub const GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE: c_int = 36;
pub const GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT: c_int = 43;

/// terminal.h のシェル統合（OSC 133。T-119）
pub const GHOSTTY_SEMANTIC_PROMPT_PROMPT_START: c_int = 1;
pub const GHOSTTY_SEMANTIC_PROMPT_INPUT_START: c_int = 2;
pub const GHOSTTY_SEMANTIC_PROMPT_OUTPUT_START: c_int = 3;
pub const GHOSTTY_SEMANTIC_PROMPT_COMMAND_END: c_int = 4;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalSemanticPrompt {
    pub kind: c_int,
    pub prompt_kind: c_int,
    pub has_exit_code: bool,
    pub exit_code: i32,
    pub command: GhosttyString,
    pub error: GhosttyString,
}
/// screen.h の行・セルの種別（OSC 133。T-119）
pub const GHOSTTY_ROW_DATA_SEMANTIC_PROMPT: c_int = 6;
pub const GHOSTTY_ROW_SEMANTIC_NONE: c_int = 0;
pub const GHOSTTY_ROW_SEMANTIC_PROMPT: c_int = 1;
pub const GHOSTTY_ROW_SEMANTIC_PROMPT_CONTINUATION: c_int = 2;
pub const GHOSTTY_CELL_DATA_SEMANTIC_CONTENT: c_int = 9;
pub const GHOSTTY_CELL_SEMANTIC_OUTPUT: c_int = 0;
pub const GHOSTTY_CELL_SEMANTIC_INPUT: c_int = 1;
pub const GHOSTTY_CELL_SEMANTIC_PROMPT: c_int = 2;
pub const GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES: c_int = 37;

/// terminal.h の未知のシーケンス（T-118）
pub const GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_APC: c_int = 0;
pub const GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_OSC: c_int = 1;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalUnknownStringSequence {
    pub truncated: bool,
    pub content: GhosttyString,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalUnknownOscSequence {
    pub truncated: bool,
    pub content: GhosttyString,
    pub terminator: c_int,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyTerminalUnknownSequenceValue {
    pub apc: GhosttyTerminalUnknownStringSequence,
    pub osc: GhosttyTerminalUnknownOscSequence,
    pub _padding: [u64; 16],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalUnknownSequence {
    pub tag: c_int,
    pub value: GhosttyTerminalUnknownSequenceValue,
}

// GhosttyTerminalData
pub const GHOSTTY_TERMINAL_DATA_COLS: c_int = 1;
pub const GHOSTTY_TERMINAL_DATA_ROWS: c_int = 2;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_X: c_int = 3;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_Y: c_int = 4;
pub const GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN: c_int = 6;
pub const GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS: c_int = 8;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBAR: c_int = 9;
pub const GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING: c_int = 11;
pub const GHOSTTY_TERMINAL_DATA_TITLE: c_int = 12;
pub const GHOSTTY_TERMINAL_DATA_PWD: c_int = 13;
pub const GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT: c_int = 25;
pub const GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS: c_int = 30;
pub const GHOSTTY_TERMINAL_DATA_SELECTION: c_int = 31;
pub const GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE: c_int = 32;
pub const GHOSTTY_TERMINAL_DATA_MODE: c_int = 37;

// GhosttyRenderStateData
pub const GHOSTTY_RENDER_STATE_DATA_COLS: c_int = 1;
pub const GHOSTTY_RENDER_STATE_DATA_ROWS: c_int = 2;
pub const GHOSTTY_RENDER_STATE_DATA_DIRTY: c_int = 3;
pub const GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR: c_int = 4;
pub const GHOSTTY_RENDER_STATE_DATA_CURSOR: c_int = 18;
pub const GHOSTTY_RENDER_STATE_DATA_COLORS: c_int = 19;
pub const GHOSTTY_RENDER_STATE_DATA_OVERSCAN: c_int = 20;
pub const GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR: c_int = 22;

// GhosttyRenderStateOption
pub const GHOSTTY_RENDER_STATE_OPT_OVERSCAN: c_int = 2;

// GhosttyRenderStateDirty
pub const GHOSTTY_RENDER_STATE_DIRTY_FALSE: c_int = 0;
pub const GHOSTTY_RENDER_STATE_DIRTY_PARTIAL: c_int = 1;
pub const GHOSTTY_RENDER_STATE_DIRTY_FULL: c_int = 2;

// GhosttyRenderStateRowData
pub const GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY: c_int = 1;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_CELLS: c_int = 3;
pub const GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION: c_int = 4;

// GhosttyRenderStateRowCellsData
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW: c_int = 1;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE: c_int = 2;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN: c_int = 3;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF: c_int = 4;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR: c_int = 5;
pub const GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR: c_int = 6;

// GhosttyImageProtocol / GhosttyImageLayer / GhosttyRenderStateImageData（render.h、types.h）
pub const GHOSTTY_IMAGE_PROTOCOL_KITTY: c_int = 0;
pub const GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND: c_int = 0;
pub const GHOSTTY_IMAGE_LAYER_BELOW_TEXT: c_int = 1;
pub const GHOSTTY_IMAGE_LAYER_ABOVE_TEXT: c_int = 2;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL: c_int = 1;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID: c_int = 2;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION: c_int = 3;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER: c_int = 4;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_Z: c_int = 5;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL: c_int = 6;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW: c_int = 7;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X: c_int = 10;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y: c_int = 11;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH: c_int = 12;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT: c_int = 13;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X: c_int = 14;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y: c_int = 15;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH: c_int = 16;
pub const GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT: c_int = 17;

// GhosttyKittyImageFormat / GhosttyKittyGraphicsImageData（kitty_graphics.h）
pub const GHOSTTY_KITTY_IMAGE_FORMAT_RGB: c_int = 0;
pub const GHOSTTY_KITTY_IMAGE_FORMAT_RGBA: c_int = 1;
pub const GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA: c_int = 3;
pub const GHOSTTY_KITTY_IMAGE_FORMAT_GRAY: c_int = 4;
pub const GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH: c_int = 3;
pub const GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT: c_int = 4;
pub const GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT: c_int = 5;
pub const GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS: c_int = 7;
pub const GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_GENERATION: c_int = 9;

// GhosttySysOption（sys.h）
pub const GHOSTTY_SYS_OPT_DECODE_PNG: c_int = 2;

// GhosttyFormatterFormat（types.h）/ GhosttyFormatterOption（formatter.h）
pub const GHOSTTY_FORMATTER_FORMAT_PLAIN: c_int = 0;
pub const GHOSTTY_FORMATTER_OPT_EMIT: c_int = 1;
pub const GHOSTTY_FORMATTER_OPT_UNWRAP: c_int = 2;
pub const GHOSTTY_FORMATTER_OPT_TRIM: c_int = 3;
pub const GHOSTTY_FORMATTER_OPT_SELECTION: c_int = 4;

// GhosttySelectionGestureEventType / Option（selection.h）
pub const GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_PRESS: c_int = 0;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_RELEASE: c_int = 1;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DRAG: c_int = 2;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF: c_int = 1;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION: c_int = 2;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_OPT_TIME_NS: c_int = 4;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REPEAT_INTERVAL_NS: c_int = 5;
pub const GHOSTTY_SELECTION_GESTURE_EVENT_OPT_GEOMETRY: c_int = 9;

// GhosttyMouseAction / GhosttyMouseButton（mouse/event.h）/ GhosttyMouseEncoderOption
pub const GHOSTTY_MOUSE_ACTION_PRESS: c_int = 0;
pub const GHOSTTY_MOUSE_ACTION_RELEASE: c_int = 1;
pub const GHOSTTY_MOUSE_ACTION_MOTION: c_int = 2;
pub const GHOSTTY_MOUSE_BUTTON_LEFT: c_int = 1;
pub const GHOSTTY_MOUSE_BUTTON_RIGHT: c_int = 2;
pub const GHOSTTY_MOUSE_BUTTON_MIDDLE: c_int = 3;
pub const GHOSTTY_MOUSE_BUTTON_FOUR: c_int = 4;
pub const GHOSTTY_MOUSE_BUTTON_FIVE: c_int = 5;
pub const GHOSTTY_MOUSE_ENCODER_OPT_SIZE: c_int = 3;
pub const GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED: c_int = 4;
pub const GHOSTTY_MOUSE_ENCODER_OPT_TRACK_LAST_CELL: c_int = 5;

// GhosttyBuildInfo（build_info.h）
pub const GHOSTTY_BUILD_INFO_VERSION_STRING: c_int = 5;

// GhosttyCellData / GhosttyCellWide（screen.h）
pub type GhosttyCell = u64;
pub const GHOSTTY_CELL_DATA_WIDE: c_int = 3;
pub const GHOSTTY_CELL_WIDE_NARROW: c_int = 0;
pub const GHOSTTY_CELL_WIDE_WIDE: c_int = 1;
pub const GHOSTTY_CELL_WIDE_SPACER_TAIL: c_int = 2;
pub const GHOSTTY_CELL_WIDE_SPACER_HEAD: c_int = 3;

// GhosttyCursorStyle（types.h）
pub const GHOSTTY_CURSOR_STYLE_BAR: c_int = 0;
pub const GHOSTTY_CURSOR_STYLE_BLOCK: c_int = 1;
pub const GHOSTTY_CURSOR_STYLE_UNDERLINE: c_int = 2;
pub const GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW: c_int = 3;

// GhosttyKeyAction / GhosttyMods / GhosttyKey（key/event.h）
pub const GHOSTTY_KEY_ACTION_RELEASE: c_int = 0;
pub const GHOSTTY_KEY_ACTION_PRESS: c_int = 1;
pub const GHOSTTY_KEY_ACTION_REPEAT: c_int = 2;
pub const GHOSTTY_MODS_SHIFT: u16 = 1 << 0;
pub const GHOSTTY_MODS_CTRL: u16 = 1 << 1;
pub const GHOSTTY_MODS_ALT: u16 = 1 << 2;
pub const GHOSTTY_KEY_UNIDENTIFIED: c_int = 0;
pub const GHOSTTY_KEY_BACKQUOTE: c_int = 1;
pub const GHOSTTY_KEY_BACKSLASH: c_int = 2;
pub const GHOSTTY_KEY_BRACKET_LEFT: c_int = 3;
pub const GHOSTTY_KEY_BRACKET_RIGHT: c_int = 4;
pub const GHOSTTY_KEY_COMMA: c_int = 5;
/// 数字の 0。1〜9 は続く番号（9 = 15）
pub const GHOSTTY_KEY_DIGIT_0: c_int = 6;
pub const GHOSTTY_KEY_EQUAL: c_int = 16;
pub const GHOSTTY_KEY_INTL_BACKSLASH: c_int = 17;
/// 英字の A。B〜Z は続く番号（Z = 45）
pub const GHOSTTY_KEY_A: c_int = 20;
pub const GHOSTTY_KEY_MINUS: c_int = 46;
pub const GHOSTTY_KEY_PERIOD: c_int = 47;
pub const GHOSTTY_KEY_QUOTE: c_int = 48;
pub const GHOSTTY_KEY_SEMICOLON: c_int = 49;
pub const GHOSTTY_KEY_SLASH: c_int = 50;
pub const GHOSTTY_KEY_BACKSPACE: c_int = 53;
pub const GHOSTTY_KEY_CONTEXT_MENU: c_int = 55;
pub const GHOSTTY_KEY_ENTER: c_int = 58;
pub const GHOSTTY_KEY_SPACE: c_int = 63;
pub const GHOSTTY_KEY_TAB: c_int = 64;
pub const GHOSTTY_KEY_DELETE: c_int = 68;
pub const GHOSTTY_KEY_END: c_int = 69;
pub const GHOSTTY_KEY_HOME: c_int = 71;
pub const GHOSTTY_KEY_INSERT: c_int = 72;
pub const GHOSTTY_KEY_PAGE_DOWN: c_int = 73;
pub const GHOSTTY_KEY_PAGE_UP: c_int = 74;
pub const GHOSTTY_KEY_ARROW_DOWN: c_int = 75;
pub const GHOSTTY_KEY_ARROW_LEFT: c_int = 76;
pub const GHOSTTY_KEY_ARROW_RIGHT: c_int = 77;
pub const GHOSTTY_KEY_ARROW_UP: c_int = 78;
/// テンキーの 0。1〜9 は続く番号（9 = 89）
pub const GHOSTTY_KEY_NUMPAD_0: c_int = 80;
pub const GHOSTTY_KEY_NUMPAD_ADD: c_int = 90;
pub const GHOSTTY_KEY_NUMPAD_DECIMAL: c_int = 95;
pub const GHOSTTY_KEY_NUMPAD_DIVIDE: c_int = 96;
pub const GHOSTTY_KEY_NUMPAD_ENTER: c_int = 97;
pub const GHOSTTY_KEY_NUMPAD_MULTIPLY: c_int = 104;
pub const GHOSTTY_KEY_NUMPAD_SUBTRACT: c_int = 107;
pub const GHOSTTY_KEY_NUMPAD_HOME: c_int = 114;
pub const GHOSTTY_KEY_NUMPAD_END: c_int = 115;
pub const GHOSTTY_KEY_NUMPAD_INSERT: c_int = 116;
pub const GHOSTTY_KEY_NUMPAD_DELETE: c_int = 117;
pub const GHOSTTY_KEY_NUMPAD_PAGE_UP: c_int = 118;
pub const GHOSTTY_KEY_NUMPAD_PAGE_DOWN: c_int = 119;
pub const GHOSTTY_KEY_NUMPAD_UP: c_int = 109;
pub const GHOSTTY_KEY_NUMPAD_DOWN: c_int = 110;
pub const GHOSTTY_KEY_NUMPAD_RIGHT: c_int = 111;
pub const GHOSTTY_KEY_NUMPAD_LEFT: c_int = 112;
pub const GHOSTTY_KEY_ESCAPE: c_int = 120;
/// F1。F2〜F25 は続く番号（F12 = 132、F24 = 144）
pub const GHOSTTY_KEY_F1: c_int = 121;
pub const GHOSTTY_KEY_PRINT_SCREEN: c_int = 148;
pub const GHOSTTY_KEY_SCROLL_LOCK: c_int = 149;
pub const GHOSTTY_KEY_PAUSE: c_int = 150;

// ---- コールバックの型（terminal.h） ----
pub type GhosttyTerminalWritePtyFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, data: *const u8, len: usize);
pub type GhosttyTerminalBellFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void);
pub type GhosttyTerminalTitleChangedFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void);
pub type GhosttyTerminalPwdChangedFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void);
pub type GhosttyTerminalXtversionFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void) -> GhosttyString;
pub type GhosttyTerminalRenderHoldFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, held: bool);
pub type GhosttyTerminalSizeFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, out: *mut GhosttySizeReportSize) -> bool;
pub type GhosttyClipboardWriteReplyFn =
    unsafe extern "C" fn(write: *const GhosttyClipboardWrite, reply: *const GhosttyClipboardWriteReply);
pub type GhosttyClipboardReadReplyFn =
    unsafe extern "C" fn(read: *const GhosttyClipboardRead, reply: *const GhosttyClipboardReadReply);
pub type GhosttyTerminalClipboardWriteFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, write: *const GhosttyClipboardWrite);
pub type GhosttyTerminalClipboardReadFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, read: *const GhosttyClipboardRead);
pub type GhosttyTerminalDesktopNotificationFn = unsafe extern "C" fn(
    terminal: GhosttyTerminal,
    userdata: *mut c_void,
    notification: *const GhosttyTerminalDesktopNotification,
);
/// `CSI ? 996 n`（配色の問い合わせ）に答える。`out` は GhosttyColorScheme（T-102）
/// シェル統合の知らせ（OSC 133。T-119）
pub type GhosttyTerminalSemanticPromptFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, event: *const GhosttyTerminalSemanticPrompt);
/// 未知のシーケンス（APC・OSC）を受ける（T-118）
pub type GhosttyTerminalUnknownSequenceFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, sequence: *const GhosttyTerminalUnknownSequence);
pub type GhosttyTerminalColorSchemeFn = unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, out: *mut c_int) -> bool;
pub const GHOSTTY_COLOR_SCHEME_LIGHT: c_int = 0;
pub const GHOSTTY_COLOR_SCHEME_DARK: c_int = 1;
pub type GhosttyTerminalProgressReportFn =
    unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, report: *const GhosttyTerminalProgressReport);

// ---- コールバックの型（sys.h） ----
pub type GhosttySysDecodePngFn = unsafe extern "C" fn(
    userdata: *mut c_void,
    allocator: *const GhosttyAllocator,
    data: *const u8,
    data_len: usize,
    out: *mut GhosttySysImage,
) -> bool;

unsafe extern "C" {
    pub fn ghostty_type_json() -> *const c_char;

    // build_info.h
    pub fn ghostty_build_info(data: c_int, out: *mut c_void) -> GhosttyResult;

    // terminal.h
    pub fn ghostty_terminal_new(
        allocator: *const GhosttyAllocator,
        terminal: *mut GhosttyTerminal,
        cols: u16,
        rows: u16,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_free(terminal: GhosttyTerminal);
    pub fn ghostty_terminal_resize(
        terminal: GhosttyTerminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_set(terminal: GhosttyTerminal, option: c_int, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_terminal_vt_write(terminal: GhosttyTerminal, data: *const u8, len: usize);
    pub fn ghostty_terminal_scroll_viewport(
        terminal: GhosttyTerminal,
        behavior: GhosttyTerminalScrollViewport,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_get(terminal: GhosttyTerminal, data: c_int, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref(
        terminal: GhosttyTerminal,
        point: GhosttyPoint,
        out_ref: *mut GhosttyGridRef,
    ) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref_track(
        terminal: GhosttyTerminal,
        point: GhosttyPoint,
        out_ref: *mut GhosttyTrackedGridRef,
    ) -> GhosttyResult;

    // grid_ref.h、screen.h（行）
    pub fn ghostty_grid_ref_row(grid_ref: *const GhosttyGridRef, out_row: *mut GhosttyRow) -> GhosttyResult;
    pub fn ghostty_grid_ref_cell(grid_ref: *const GhosttyGridRef, out_cell: *mut GhosttyCell) -> GhosttyResult;
    pub fn ghostty_row_get(row: GhosttyRow, data: c_int, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_grid_ref_hyperlink_uri(
        grid_ref: *const GhosttyGridRef,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;

    // grid_ref_tracked.h
    pub fn ghostty_tracked_grid_ref_free(tracked: GhosttyTrackedGridRef);
    pub fn ghostty_tracked_grid_ref_point(
        tracked: GhosttyTrackedGridRef,
        tag: c_int,
        out_point: *mut GhosttyPointCoordinate,
    ) -> GhosttyResult;

    // formatter.h、allocator.h
    pub fn ghostty_formatter_terminal_new(
        allocator: *const GhosttyAllocator,
        formatter: *mut GhosttyFormatter,
        terminal: GhosttyTerminal,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_set(formatter: GhosttyFormatter, option: c_int, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_formatter_format_alloc(
        formatter: GhosttyFormatter,
        allocator: *const GhosttyAllocator,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_formatter_free(formatter: GhosttyFormatter);
    pub fn ghostty_free(allocator: *const GhosttyAllocator, ptr: *mut u8, len: usize);
    pub fn ghostty_alloc(allocator: *const GhosttyAllocator, len: usize) -> *mut u8;

    // sys.h
    pub fn ghostty_sys_set(option: c_int, value: *const c_void) -> GhosttyResult;

    // kitty_graphics.h
    pub fn ghostty_kitty_graphics_image(graphics: GhosttyKittyGraphics, image_id: u32) -> GhosttyKittyGraphicsImage;
    pub fn ghostty_kitty_graphics_animation_tick(graphics: GhosttyKittyGraphics, now_ms: u64, next_ms: *mut u64) -> GhosttyResult;
    pub fn ghostty_kitty_graphics_image_get_multi(
        image: GhosttyKittyGraphicsImage,
        count: usize,
        keys: *const c_int,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;

    // selection.h
    pub fn ghostty_selection_gesture_new(
        allocator: *const GhosttyAllocator,
        out_gesture: *mut GhosttySelectionGesture,
    ) -> GhosttyResult;
    pub fn ghostty_selection_gesture_free(gesture: GhosttySelectionGesture, terminal: GhosttyTerminal);
    pub fn ghostty_selection_gesture_event_new(
        allocator: *const GhosttyAllocator,
        out_event: *mut GhosttySelectionGestureEvent,
        kind: c_int,
    ) -> GhosttyResult;
    pub fn ghostty_selection_gesture_event_free(event: GhosttySelectionGestureEvent);
    pub fn ghostty_selection_gesture_event_set(
        event: GhosttySelectionGestureEvent,
        option: c_int,
        value: *const c_void,
    ) -> GhosttyResult;
    pub fn ghostty_selection_gesture_event(
        gesture: GhosttySelectionGesture,
        terminal: GhosttyTerminal,
        event: GhosttySelectionGestureEvent,
        out_selection: *mut GhosttySelection,
    ) -> GhosttyResult;

    // mouse/encoder.h、mouse/event.h
    pub fn ghostty_mouse_encoder_new(
        allocator: *const GhosttyAllocator,
        encoder: *mut GhosttyMouseEncoder,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_free(encoder: GhosttyMouseEncoder);
    pub fn ghostty_mouse_encoder_set(encoder: GhosttyMouseEncoder, option: c_int, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_mouse_encoder_set_from_terminal(encoder: GhosttyMouseEncoder, terminal: GhosttyTerminal)
    -> GhosttyResult;
    pub fn ghostty_mouse_encoder_encode(
        encoder: GhosttyMouseEncoder,
        event: GhosttyMouseEvent,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_mouse_event_new(allocator: *const GhosttyAllocator, event: *mut GhosttyMouseEvent) -> GhosttyResult;
    pub fn ghostty_mouse_event_free(event: GhosttyMouseEvent);
    pub fn ghostty_mouse_event_set_action(event: GhosttyMouseEvent, action: c_int) -> GhosttyResult;
    pub fn ghostty_mouse_event_set_button(event: GhosttyMouseEvent, button: c_int) -> GhosttyResult;
    pub fn ghostty_mouse_event_clear_button(event: GhosttyMouseEvent) -> GhosttyResult;
    pub fn ghostty_mouse_event_set_mods(event: GhosttyMouseEvent, mods: u16) -> GhosttyResult;
    pub fn ghostty_mouse_event_set_position(event: GhosttyMouseEvent, position: GhosttyMousePosition) -> GhosttyResult;

    // paste.h
    pub fn ghostty_paste_is_safe(data: *const c_char, len: usize) -> bool;

    // render.h
    pub fn ghostty_render_state_new(allocator: *const GhosttyAllocator, state: *mut GhosttyRenderState)
    -> GhosttyResult;
    pub fn ghostty_render_state_free(state: GhosttyRenderState);
    pub fn ghostty_render_state_update(state: GhosttyRenderState, terminal: GhosttyTerminal) -> GhosttyResult;
    pub fn ghostty_render_state_clean(state: GhosttyRenderState) -> GhosttyResult;
    pub fn ghostty_render_state_set(state: GhosttyRenderState, option: c_int, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_render_state_get(state: GhosttyRenderState, data: c_int, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const GhosttyAllocator,
        out: *mut GhosttyRenderStateRowIterator,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_iterator_free(iterator: GhosttyRenderStateRowIterator);
    pub fn ghostty_render_state_row_iterator_next(iterator: GhosttyRenderStateRowIterator) -> bool;
    pub fn ghostty_render_state_row_iterator_next_dirty(
        iterator: GhosttyRenderStateRowIterator,
        out_y: *mut u16,
    ) -> bool;
    pub fn ghostty_render_state_row_get(
        iterator: GhosttyRenderStateRowIterator,
        data: c_int,
        out: *mut c_void,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_new(
        allocator: *const GhosttyAllocator,
        out: *mut GhosttyRenderStateRowCells,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_row_cells_free(cells: GhosttyRenderStateRowCells);
    pub fn ghostty_render_state_row_cells_next(cells: GhosttyRenderStateRowCells) -> bool;
    pub fn ghostty_render_state_row_cells_get(
        cells: GhosttyRenderStateRowCells,
        data: c_int,
        out: *mut c_void,
    ) -> GhosttyResult;

    pub fn ghostty_render_state_image_iterator_new(
        allocator: *const GhosttyAllocator,
        out_iterator: *mut GhosttyRenderStateImageIterator,
    ) -> GhosttyResult;
    pub fn ghostty_render_state_image_iterator_free(iterator: GhosttyRenderStateImageIterator);
    pub fn ghostty_render_state_image_iterator_next(iterator: GhosttyRenderStateImageIterator) -> bool;
    pub fn ghostty_render_state_image_get_multi(
        iterator: GhosttyRenderStateImageIterator,
        count: usize,
        keys: *const c_int,
        values: *mut *mut c_void,
        out_written: *mut usize,
    ) -> GhosttyResult;

    // screen.h
    pub fn ghostty_cell_get(cell: GhosttyCell, data: c_int, out: *mut c_void) -> GhosttyResult;

    // key/encoder.h、key/event.h
    pub fn ghostty_key_encoder_new(allocator: *const GhosttyAllocator, encoder: *mut GhosttyKeyEncoder)
    -> GhosttyResult;
    pub fn ghostty_key_encoder_free(encoder: GhosttyKeyEncoder);
    pub fn ghostty_key_encoder_set_from_terminal(encoder: GhosttyKeyEncoder, terminal: GhosttyTerminal)
    -> GhosttyResult;
    pub fn ghostty_key_encoder_encode(
        encoder: GhosttyKeyEncoder,
        event: GhosttyKeyEvent,
        buf: *mut u8,
        buf_len: usize,
        out_written: *mut usize,
    ) -> GhosttyResult;
    pub fn ghostty_key_event_new(allocator: *const GhosttyAllocator, event: *mut GhosttyKeyEvent) -> GhosttyResult;
    pub fn ghostty_key_event_free(event: GhosttyKeyEvent);
    pub fn ghostty_key_event_set_action(event: GhosttyKeyEvent, action: c_int) -> GhosttyResult;
    pub fn ghostty_key_event_set_key(event: GhosttyKeyEvent, key: c_int) -> GhosttyResult;
    pub fn ghostty_key_event_set_mods(event: GhosttyKeyEvent, mods: u16) -> GhosttyResult;
    pub fn ghostty_key_event_set_consumed_mods(event: GhosttyKeyEvent, consumed_mods: u16) -> GhosttyResult;
    pub fn ghostty_key_event_set_utf8(event: GhosttyKeyEvent, utf8: GhosttyString) -> GhosttyResult;
    pub fn ghostty_key_event_set_unshifted_codepoint(event: GhosttyKeyEvent, codepoint: u32) -> GhosttyResult;
    pub fn ghostty_key_event_set_composing(event: GhosttyKeyEvent, composing: bool) -> GhosttyResult;

    // unicode.h
    pub fn ghostty_unicode_codepoint_width(cp: u32) -> u8;
}
