//! コマンドライン引数。
//!
//! ```text
//! embterm.exe [--conpty-dir DIR | --system-conpty] [--font NAME] [--font-size PT] [--font-fallback NAME]... [--narrow-wide shrink|overlap] [--cols N --rows N] [--cwd DIR]
//!             [--input STR]... [--input-delay MS] [--input-interval MS]
//!             [--select C1,R1,C2,R2] [--resize C,R] [--resize-after MS] [--scroll-up N] [--screenshot PNG] [--screenshot-after MS] [--exit-after MS]
//!             [--log FILE] [--perf] [--exit-on-exit] [-- コマンド 引数...]
//! ```
//!
//! `--input` 以降は自動で確かめるためのもの（人手なしで「起動 → 打つ → 画面を PNG に → 閉じる」）。

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct Args {
    pub conpty_dir: Option<PathBuf>,
    /// 再配布版を探さず、OS 同梱の ConPTY（kernel32）を使う（見比べるため）
    pub system_conpty: bool,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    /// 主フォントに無い文字に、システムのフォールバックより先に使うファミリ（T-033）
    pub font_fallback: Vec<String>,
    /// 幅 1 と数えたのにグリフが広い文字の描き方（T-013）。None なら既定（overlap）
    pub narrow_wide: Option<et_render::NarrowWide>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub cwd: Option<PathBuf>,
    /// 起動して `input_delay` ms 後から、`input_interval` ms おきに 1 つずつ PTY に送る（エスケープは解釈済み）
    pub inputs: Vec<Vec<u8>>,
    pub input_delay: u32,
    pub input_interval: u32,
    /// 画面を保存する直前に選択範囲にする（桁, 行）の組
    pub select: Option<((u16, u16), (u16, u16))>,
    /// `resize_after` ms 後にウィンドウの大きさを（桁, 行）に変える（リサイズの確認用）
    pub resize: Option<(u16, u16)>,
    pub resize_after: u32,
    /// 画面を保存する直前にスクロールバックを何行遡るか（ホイールと同じ経路の確認用）
    pub scroll_up: Option<u16>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_after: Option<u32>,
    pub exit_after: Option<u32>,
    pub log: Option<PathBuf>,
    /// 終わるときに性能の測定（T-054）をログに書く
    pub perf: bool,
    /// 子プロセスが終わったらウィンドウを閉じる（`cat` の所要時間の測定用。T-054）
    pub exit_on_exit: bool,
    /// 起動するコマンド。空なら cmd.exe
    pub command: Vec<OsString>,
}

pub const USAGE: &str = "使い方: embterm.exe [--conpty-dir DIR | --system-conpty] [--font NAME] [--font-size PT] [--font-fallback NAME]... [--narrow-wide shrink|overlap] [--cols N --rows N] [--cwd DIR]
                   [--input STR]... [--input-delay MS] [--input-interval MS]
                   [--select C1,R1,C2,R2] [--resize C,R] [--resize-after MS] [--scroll-up N] [--screenshot PNG] [--screenshot-after MS] [--exit-after MS]
                   [--log FILE] [--perf] [--exit-on-exit] [-- コマンド 引数...]
  --input の文字列は \\r \\n \\t \\e（ESC） \\\\ \\xHH \\u{HHHH} を解釈する";

/// `\r` `\n` `\t` `\e` `\\` `\xHH`（ASCII） `\u{H..}` を解釈して UTF-8 のバイト列にする。知らないものはそのまま残す
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('a') => out.push('\x07'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
                let v = u8::from_str_radix(&h, 16).map_err(|_| format!("\\x の後が 16 進 2 桁でない: {h}"))?;
                out.push(v as char);
            }
            Some('u') => {
                if it.next() != Some('{') {
                    return Err("\\u の後は {…}".into());
                }
                let h: String = it.by_ref().take_while(|&c| c != '}').collect();
                let v = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32).ok_or(format!("\\u{{{h}}} が文字でない"))?;
                out.push(v);
            }
            // 知らないものはそのまま残す（C:\Users のようなパスを書けるように）
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    Ok(out.into_bytes())
}

fn num<T: std::str::FromStr>(name: &str, v: Option<String>) -> Result<T, String> {
    let v = v.ok_or(format!("{name} に値が要る"))?;
    v.parse().map_err(|_| format!("{name} の値が数でない: {v}"))
}

pub fn parse(mut it: impl Iterator<Item = OsString>) -> Result<Args, String> {
    let mut a = Args { input_delay: 1500, input_interval: 400, resize_after: 1000, ..Default::default() };
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy().into_owned();
        let mut val = || it.next().map(|v| v.to_string_lossy().into_owned());
        match s.as_str() {
            "--" => {
                a.command = it.collect();
                break;
            }
            "--conpty-dir" => a.conpty_dir = Some(val().ok_or("--conpty-dir に値が要る")?.into()),
            "--system-conpty" => a.system_conpty = true,
            "--font" => a.font = Some(val().ok_or("--font に値が要る")?),
            "--font-size" => a.font_size = Some(num("--font-size", val())?),
            "--font-fallback" => a.font_fallback.push(val().ok_or("--font-fallback に値が要る")?),
            "--narrow-wide" => a.narrow_wide = Some(val().ok_or("--narrow-wide に値が要る")?.parse()?),
            "--cols" => a.cols = Some(num("--cols", val())?),
            "--rows" => a.rows = Some(num("--rows", val())?),
            "--cwd" => a.cwd = Some(val().ok_or("--cwd に値が要る")?.into()),
            "--input" => a.inputs.push(unescape(&val().ok_or("--input に値が要る")?)?),
            "--input-delay" => a.input_delay = num("--input-delay", val())?,
            "--input-interval" => a.input_interval = num("--input-interval", val())?,
            "--screenshot" => a.screenshot = Some(val().ok_or("--screenshot に値が要る")?.into()),
            "--screenshot-after" => a.screenshot_after = Some(num("--screenshot-after", val())?),
            "--exit-after" => a.exit_after = Some(num("--exit-after", val())?),
            "--log" => a.log = Some(val().ok_or("--log に値が要る")?.into()),
            "--perf" => a.perf = true,
            "--exit-on-exit" => a.exit_on_exit = true,
            "--select" => {
                let v = val().ok_or("--select に値が要る")?;
                let n: Vec<u16> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().map_err(|_| format!("--select の値: {v}"))?;
                if n.len() != 4 {
                    return Err(format!("--select は C1,R1,C2,R2: {v}"));
                }
                a.select = Some(((n[0], n[1]), (n[2], n[3])));
            }
            "--resize" => {
                let v = val().ok_or("--resize に値が要る")?;
                let n: Vec<u16> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().map_err(|_| format!("--resize の値: {v}"))?;
                if n.len() != 2 {
                    return Err(format!("--resize は C,R: {v}"));
                }
                a.resize = Some((n[0], n[1]));
            }
            "--scroll-up" => a.scroll_up = Some(num("--scroll-up", val())?),
            "--resize-after" => a.resize_after = num("--resize-after", val())?,
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("知らない引数 {s}\n{USAGE}")),
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"echo a\r").unwrap(), b"echo a\r");
        assert_eq!(unescape(r"\e[A\x41\u{2460}\\").unwrap(), "\x1b[AA①\\".as_bytes());
        assert_eq!(unescape(r"dir C:\Users").unwrap(), br"dir C:\Users");
    }

    #[test]
    fn parse_args() {
        let a = parse(["--cols", "80", "--input", r"dir\r", "--", "pwsh", "-NoLogo"].into_iter().map(OsString::from)).unwrap();
        assert_eq!(a.cols, Some(80));
        assert_eq!(a.inputs, vec![b"dir\r".to_vec()]);
        assert_eq!(a.command, vec![OsString::from("pwsh"), OsString::from("-NoLogo")]);
    }
}
