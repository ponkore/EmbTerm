//! フレームのどこをペインに描くか（T-063）。
//!
//! ペインの大きさ（桁・行）は端末の大きさと違うことがある。フレームは端末の行（と overscan）を持ち、
//! ペインはそこから自分の行だけを切り取って描く。

use crate::frame::Frame;

/// フレームの一部を描くための指定
#[derive(Clone, Copy, Debug)]
pub struct FrameView {
    /// ペインの一番上の行が、`Frame` の何行目か（負なら上に足りない行は空き）
    pub top: i32,
    /// ペインの桁数（フレームより狭ければ右を切り取る）
    pub cols: u16,
    /// ペインの行数
    pub rows: u16,
    /// カーソルを描くか（スクロールバックを遡っているペインでは false）
    pub show_cursor: bool,
}

impl FrameView {
    /// フレーム全体を描く（embterm.exe 用）
    pub fn whole(frame: &Frame) -> Self {
        Self { top: 0, cols: frame.cols, rows: frame.rows, show_cursor: true }
    }
}
