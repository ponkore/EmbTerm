//! IME の候補ウィンドウの位置を端末のカーソルに合わせる（T-129）。
//!
//! macOS の入力メソッドは、候補ウィンドウ（変換候補の一覧）を
//! `-[NSTextInputClient firstRectForCharacterRange:actualRange:]` が返した矩形の下に出す。
//! Emacs の `EmacsView` はこれに**バッファのポイントの位置**を返すので、端末のバッファでは
//! 置き物のテキストの位置（たいていウィンドウの左上）に出てしまう。
//!
//! そこで **`EmacsView` の `firstRectForCharacterRange:actualRange:` を差し替え**（method swizzling）、
//! 端末を選んでいる間だけ端末のカーソルの矩形を返す。Windows の tr-ime の
//! `tr-ime-modadv--set-composition-window`（T-010）に当たるもの。
//!
//! 安全のための決まり
//! - 差し替えは**プロセスで 1 回だけ**。元の実装は覚えておき、端末を選んでいないときや
//!   別のビューのときは**必ず元の実装に渡す**
//! - 返す矩形は「どのビューの、ビュー座標のどこか」で覚える。画面座標への変換は
//!   差し替えた実装の中（= main スレッド）で行う
//! - 置く場所が分からなくなったら（ペインを消した・フォーカスが外れた）`clear` して元の動きに戻す

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use objc2::ffi::{class_getInstanceMethod, method_getImplementation, method_setImplementation, objc_getClass};
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2_app_kit::NSView;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSRange;

use crate::log;

type RectFn = unsafe extern "C" fn(*mut AnyObject, Sel, NSRange, *mut NSRange) -> CGRect;

/// 元の実装（`EmacsView` のもの）
static ORIGINAL: OnceLock<RectFn> = OnceLock::new();
/// 差し替えを試したか（1 回だけ）
static TRIED: AtomicUsize = AtomicUsize::new(0);
/// 端末のカーソル（ビューのアドレス, ビュー座標の矩形）。`None` なら元の動き
static TARGET: Mutex<Option<(usize, CGRect)>> = Mutex::new(None);

/// 差し替えた実装。端末を選んでいるビューなら端末のカーソルの矩形（画面座標）を返す
unsafe extern "C" fn first_rect(this: *mut AnyObject, sel: Sel, range: NSRange, actual: *mut NSRange) -> CGRect {
    let fallback = || unsafe {
        match ORIGINAL.get() {
            Some(f) => f(this, sel, range, actual),
            None => CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)),
        }
    };
    let want = {
        let g = TARGET.lock().unwrap_or_else(|e| e.into_inner());
        match *g {
            Some((ptr, rect)) if ptr == this as usize => Some(rect),
            _ => None,
        }
    };
    let Some(rect) = want else { return fallback() };
    // SAFETY: この呼び出しは main スレッドで、`this` は `EmacsView`（NSView の派生）
    let view: &NSView = unsafe { &*(this as *const NSView) };
    let Some(window) = view.window() else { return fallback() };
    // 変換中の文字列は矩形の下に出るので、カーソルの矩形をそのまま渡す（ビュー座標 → ウィンドウ → 画面）
    let in_window = view.convertRect_toView(rect, None);
    window.convertRectToScreen(in_window)
}

/// `EmacsView` の `firstRectForCharacterRange:actualRange:` を差し替える（プロセスで 1 回だけ）。
/// できたら true（すでに差し替え済みでも true）
pub fn install() -> bool {
    if TRIED.swap(1, Ordering::AcqRel) == 1 {
        return ORIGINAL.get().is_some();
    }
    // SAFETY: クラスとメソッドを名前で引き、実装を差し替える。元の実装は覚えておく
    unsafe {
        let class = objc_getClass(c"EmacsView".as_ptr());
        if class.is_null() {
            log("IME: EmacsView のクラスが見つかりません（NS 版の Emacs ではない？）");
            return false;
        }
        let sel = sel!(firstRectForCharacterRange:actualRange:);
        let method = class_getInstanceMethod(class, sel);
        if method.is_null() {
            log("IME: firstRectForCharacterRange:actualRange: がありません");
            return false;
        }
        let original: RectFn = std::mem::transmute(method_getImplementation(method));
        let _ = ORIGINAL.set(original);
        method_setImplementation(method, std::mem::transmute::<RectFn, _>(first_rect));
    }
    log("IME: EmacsView の firstRectForCharacterRange: を差し替えた（T-129）");
    true
}

/// 端末のカーソルの矩形（`view` のビュー座標）を覚える。`None` で元の動きに戻す
pub fn set_rect(view: Option<&NSView>, rect: Option<CGRect>) {
    set_rect_raw(view.map(|v| v as *const NSView as usize).unwrap_or(0), rect);
}

/// ビューをアドレスで指して覚える（描画スレッドから呼ぶ。AppKit には触らない）
pub fn set_rect_raw(view: usize, rect: Option<CGRect>) {
    let mut g = TARGET.lock().unwrap_or_else(|e| e.into_inner());
    *g = match (view, rect) {
        (0, _) | (_, None) => None,
        (ptr, Some(r)) => Some((ptr, r)),
    };
}

/// 覚えている位置を忘れる（端末を選んでいない・フォーカスが無い）
pub fn clear() {
    set_rect(None, None);
}

/// いま `EmacsView` が返す矩形（画面座標）を読む（自動確認用。T-129）。
/// 差し替えが効いていれば端末のカーソルの位置、効いていなければ Emacs のポイントの位置になる
pub fn ask_view_rect(view: &NSView) -> (f64, f64, f64, f64) {
    use objc2::msg_send;
    let range = NSRange { location: 0, length: 0 };
    // SAFETY: NSTextInputClient の実装を持つビュー（EmacsView）。main スレッドから呼ぶ
    let rect: CGRect = unsafe { msg_send![view, firstRectForCharacterRange: range, actualRange: std::ptr::null_mut::<NSRange>()] };
    (rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
}

/// 差し替えが入っているか（自動確認用）
pub fn installed() -> bool {
    ORIGINAL.get().is_some()
}
