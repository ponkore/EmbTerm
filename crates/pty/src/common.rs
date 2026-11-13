//! OS をまたいで同じ形のもの（T-124）。
//!
//! 子プロセスの終わり方・書く口・閉じたときの記録・端末用の環境変数は、ConPTY（Windows）と
//! openpty（Unix）で同じにする。

use std::ffi::OsString;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// 何も指定されなかったときに起こすもの（Windows は cmd、Unix は `$SHELL`。T-124）
pub fn default_shell() -> Vec<OsString> {
    if cfg!(windows) {
        return vec!["cmd.exe".into()];
    }
    let shell = std::env::var_os("SHELL").filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    vec![shell]
}

/// 端末用に足す環境変数（`TERM=xterm-256color`、`COLORTERM=truecolor`、`TERM_PROGRAM=EmbTerm`）
///
/// あわせて `PWD` と `OLDPWD` を消す。Git Bash などから起動した親（Emacs）の `PWD` が残っていると、
/// starship（nyagos・pwsh のプロンプト）や yazi は実際の作業ディレクトリより `PWD` を信じるので、
/// 親を起動したディレクトリで開いたように見える。正しい値を入れても、nyagos のように `cd` で
/// `PWD` を書き換えないシェルでは古くなるので、入れずに消す（POSIX のシェルは自分で設定する）
pub fn terminal_env() -> Vec<(OsString, Option<OsString>)> {
    [
        ("TERM", Some("xterm-256color")),
        ("COLORTERM", Some("truecolor")),
        ("TERM_PROGRAM", Some("EmbTerm")),
        ("TERM_PROGRAM_VERSION", Some(env!("CARGO_PKG_VERSION"))),
        ("PWD", None),
        ("OLDPWD", None),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.map(Into::into)))
    .collect()
}

/// 親（Emacs を起動した端末）が自分の中のプロセスのために入れる環境変数か（T-095）。子には渡さない。
/// EmbTerm の中のアプリが「WezTerm の中にいる」などと取り違えないように（yazi は `WEZTERM_*` を見る。
/// XTVERSION で上書きされるので害は無かったが、ほかのアプリは環境変数だけで判断する）
pub(crate) fn foreign_terminal_var(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "WT_SESSION",
        "WT_PROFILE_ID",
        "TERMINAL_EMULATOR",
        "TERM_SESSION_ID",
        "ITERM_SESSION_ID",
        "ITERM_PROFILE",
        "LC_TERMINAL",
        "LC_TERMINAL_VERSION",
        "KONSOLE_VERSION",
        "VTE_VERSION",
        "COLORFGBG",
        "WINDOWID",
    ];
    const PREFIXES: &[&str] = &["WEZTERM_", "KITTY_", "ALACRITTY_", "GHOSTTY_", "VSCODE_", "CONEMU"];
    let n = name.to_uppercase();
    NAMES.contains(&n.as_str()) || PREFIXES.iter().any(|p| n.starts_with(p))
}

/// 子プロセスの終わり方
#[derive(Clone, Copy, Debug)]
pub struct ExitStatus {
    /// 終了コード。取れなければ `None`
    pub code: Option<u32>,
    /// こちらから閉じた（[`Pty::close`]）か
    pub closed_by_us: bool,
}

pub(crate) enum WriteMsg {
    Data(Vec<u8>),
    Stop,
}

/// PTY に書く口。複製して別のスレッドへ渡してよい。書く処理は書きのスレッドが行うので、送るだけで待たない
#[derive(Clone)]
pub struct PtyWriter {
    pub(crate) tx: Sender<WriteMsg>,
}

impl PtyWriter {
    /// 送る。書きのスレッドがもう終わっていれば false
    pub fn write(&self, data: &[u8]) -> bool {
        data.is_empty() || self.tx.send(WriteMsg::Data(data.to_vec())).is_ok()
    }
}

/// 閉じたときの段取りの記録（T-076 の調べ用）。時間は閉じ始めてから
#[derive(Clone, Debug, Default)]
pub struct CloseReport {
    /// 穏やかに終わるよう頼み終えた（Windows: `ClosePseudoConsole` が戻った = CTRL_CLOSE_EVENT。
    /// Unix: プロセスグループに SIGHUP を送った）
    pub console_closed: Option<Duration>,
    /// 出力を EOF まで読み終えた
    pub eof: Option<Duration>,
    /// 子と子孫（Windows: Job の中と外、Unix: プロセスグループと子孫）がすべて終わった。時限を過ぎたら `None`
    pub all_exited: Option<Duration>,
    /// 時限を過ぎて止めたプロセスの数
    pub forced: usize,
    /// 閉じ始めたときに Job の外にいた子孫の数（Unix では常に 0。プロセスグループで束ねる）
    pub outside: usize,
}

impl std::fmt::Display for CloseReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = |d: Option<Duration>| d.map_or("-".to_string(), |d| format!("{} ms", d.as_millis()));
        write!(
            f,
            "終了の依頼 {} / EOF {} / 全員の終了 {} / Job の外の子孫 {} / 強制終了 {}",
            ms(self.console_closed),
            ms(self.eof),
            ms(self.all_exited),
            self.outside,
            self.forced
        )
    }
}
