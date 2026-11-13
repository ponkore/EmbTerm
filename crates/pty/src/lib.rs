//! et-pty: 子プロセスを PTY で動かす（docs/TODO.md T-007 / T-124）。
//!
//! OS ごとに実装が別で、外から見える形（[`Pty`]・[`PtyOptions`]・[`ExitStatus`]・[`CloseReport`]・
//! [`PtyWriter`]・[`ProcessWatch`]）をそろえてある。
//!
//! | OS | 実装 | 中身 |
//! |---|---|---|
//! | Windows | [`conpty`] | ConPTY（再配布版の `conpty.dll` を優先し、無ければ OS 同梱）+ Job Object |
//! | Unix（macOS） | [`unix`] | `openpty` + `setsid` + `TIOCSCTTY`。閉じるときはプロセスグループに SIGHUP → 猶予 → SIGKILL |
//!
//! 違い（T-124 の記録は docs/notes/t124-macos-stage0.md）
//! - Unix には ConPTY が無いので、[`PtyOptions::conpty_search`] と [`PtyOptions::flags`] は**使わない**
//!   （呼ぶ側を OS で分けずに済むよう、欄は残してある）
//! - Unix では「ConPTY が画面を組み直す」ことが無いので、文字幅の食い違い（T-011）は起きない
//! - 子孫の扱いは Job Object ではなくプロセスグループ（セッション）

mod common;
#[cfg(windows)]
mod conpty;
#[cfg(unix)]
mod unix;

pub use common::{CloseReport, ExitStatus, PtyWriter, default_shell, terminal_env};

#[cfg(windows)]
pub use conpty::*;
#[cfg(unix)]
pub use unix::*;

/// どの PTY で動いているか。Windows では ConPTY の版（再配布版か OS 同梱か）、Unix では `openpty`
#[cfg(windows)]
pub use conpty::ConptyKind as PtyKind;

/// 読み書きの結果
pub type Result<T> = std::io::Result<T>;
