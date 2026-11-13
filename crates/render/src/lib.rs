//! et-render: 端末の画面を描く。
//!
//! 中身は OS で分かれる（T-125）。
//!
//! | OS | モジュール | 中身 |
//! |---|---|---|
//! | Windows | `d2d` | HWND に DXGI のフリップモデル + Direct2D / DirectWrite（T-008） |
//! | macOS | `mac` | CAMetalLayer（かオフスクリーンのテクスチャ）に Metal + CoreText のグリフアトラス（T-125） |
//!
//! OS に依らない部分はここに置く:
//!
//! - [`Frame`]: 描く中身。libghostty-vt の render state から変わった行だけを写す（[`Frame::update`]）
//! - [`FrameView`]: フレームのどこをペインに描くか（T-063）
//! - [`Theme`] / [`NarrowWide`]: 色・余白・広いグリフの描き方（T-013）
//! - [`layout`]: 色の解決・カーソルの位置・縮める比（[`layout::row_colors`] など）
//! - [`png`]: デバッグ用の画面の保存（圧縮しない PNG）

mod frame;
mod layout;
mod png;
mod theme;
mod view;

#[cfg(windows)]
mod d2d;
#[cfg(target_os = "macos")]
mod mac;

pub use frame::{Cell, Frame, ImagePixels, PLACEHOLDER, PlacedImage};
pub use theme::{NarrowWide, Theme};
pub use view::FrameView;

#[cfg(windows)]
pub use d2d::*;
#[cfg(target_os = "macos")]
pub use mac::*;
