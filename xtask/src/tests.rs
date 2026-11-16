//! 自動確認（T-078 / T-081 / T-114）。
//!
//! - `test-screens`: embterm に入力を送って画面のテキストを撮り、test/screens/<名前>.txt と比べる
//!   （ケースの一覧は Windows が cases.txt、Unix が cases-unix.txt。T-124）
//! - `test-emacs`: 別に起こした `emacs -Q` で test/*.el を動かし、終了コードとログの NG を見る

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::root;

/// 画面の回帰テストの 1 ケース
#[derive(Clone, Debug)]
struct Case {
    name: String,
    cols: u16,
    rows: u16,
    delay: u32,
    interval: u32,
    settle: u32,
    inputs: Vec<String>,
    command: Vec<String>,
}

impl Case {
    /// 撮る時刻（起動から ms）
    fn shot_at(&self) -> u32 {
        self.delay + self.interval * (self.inputs.len().saturating_sub(1) as u32) + self.settle
    }
}

/// test/screens/cases.txt を読む
fn parse_cases(text: &str) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    let mut group: Option<Case> = None;
    for (no, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |m: &str| format!("cases.txt の {} 行目: {m}", no + 1);
        if let Some(rest) = line.strip_prefix('@') {
            let (head, cmd) = rest.split_once(" -- ").ok_or_else(|| err("`-- コマンド` がありません"))?;
            let f: Vec<&str> = head.split_whitespace().collect();
            if f.len() != 6 {
                return Err(err("@グループ 桁 行 最初の入力まで 間隔 撮るまで の 6 つが要ります"));
            }
            let n = |s: &str| s.parse::<u32>().map_err(|_| err(&format!("数ではありません: {s}")));
            group = Some(Case {
                name: f[0].to_string(),
                cols: n(f[1])? as u16,
                rows: n(f[2])? as u16,
                delay: n(f[3])?,
                interval: n(f[4])?,
                settle: n(f[5])?,
                inputs: Vec::new(),
                command: cmd.split_whitespace().map(str::to_string).collect(),
            });
            continue;
        }
        let g = group.as_ref().ok_or_else(|| err("@グループ より前にケースがあります"))?;
        let mut fields = line.split('\t');
        let head = fields.next().unwrap_or_default();
        let mut words = head.split_whitespace();
        let mut case = g.clone();
        case.name = words.next().ok_or_else(|| err("名前がありません"))?.to_string();
        for w in words {
            let v = w.strip_prefix("settle=").ok_or_else(|| err(&format!("知らない指定: {w}")))?;
            case.settle = v.parse().map_err(|_| err(&format!("数ではありません: {v}")))?;
        }
        case.inputs = fields.map(str::to_string).collect();
        cases.push(case);
    }
    Ok(cases)
}

/// 比べるための形: 行末の空白と末尾の空の行を除き、改行は LF
fn normalize(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
    let mut s = lines[..end].join("\n");
    s.push('\n');
    s
}

fn run_case(exe: &Path, out: &Path, case: &Case) -> Result<String, String> {
    let png = out.join(format!("{}.png", case.name));
    let txt = out.join(format!("{}.txt", case.name));
    let _ = std::fs::remove_file(&txt);
    let shot = case.shot_at();
    let mut cmd = Command::new(exe);
    if !cfg!(windows) {
        // macOS は窓を開かず、オフスクリーンに描いて PNG と画面のテキストを書く（T-125）
        cmd.arg("--headless");
    }
    cmd.args(["--cols", &case.cols.to_string(), "--rows", &case.rows.to_string()])
        .args(["--input-delay", &case.delay.to_string(), "--input-interval", &case.interval.to_string()]);
    for i in &case.inputs {
        cmd.arg("--input").arg(i);
    }
    cmd.arg("--screenshot")
        .arg(&png)
        .args(["--screenshot-after", &shot.to_string(), "--exit-after", &(shot + 500).to_string()])
        .arg("--log")
        .arg(out.join(format!("{}.log", case.name)))
        .arg("--")
        .args(&case.command)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = wait_with_timeout(cmd, Duration::from_millis(shot as u64 + 30_000))?;
    if !status {
        return Err(format!("{}: embterm が失敗しました", case.name));
    }
    let text = std::fs::read_to_string(&txt).map_err(|e| format!("{}: {e}", txt.display()))?;
    Ok(normalize(&text))
}

/// 子を起こして待つ。時間を過ぎたら止めて失敗にする。成功なら true
fn wait_with_timeout(mut cmd: Command, timeout: Duration) -> Result<bool, String> {
    let mut child = cmd.spawn().map_err(|e| format!("{cmd:?}: {e}"))?;
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(st.success());
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{cmd:?} が {} 秒で終わりません", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 期待値との違いを短く示す（最初に違う行の前後）
fn describe_diff(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    let i = (0..e.len().max(a.len())).find(|&i| e.get(i) != a.get(i)).unwrap_or(0);
    let mut s = format!("  {} 行目から違う\n", i + 1);
    for j in i..(i + 3).min(e.len().max(a.len())) {
        s += &format!("    期待 {:>3}| {}\n    実際 {:>3}| {}\n", j + 1, e.get(j).unwrap_or(&"（無い）"), j + 1, a.get(j).unwrap_or(&"（無い）"));
    }
    s
}

pub fn test_screens(args: &[String]) -> Result<(), String> {
    let bless = args.iter().any(|a| a == "--bless");
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if !args.iter().any(|a| a == "--no-dist") {
        crate::dist()?;
    }
    let root = root();
    let dir = root.join("test/screens");
    // ケースの一覧は OS で違う（Windows は cmd と vttest、Unix は test/screens/unix/*.sh。T-124）
    let cases_file = if cfg!(windows) { "cases.txt" } else { "cases-unix.txt" };
    let text = std::fs::read_to_string(dir.join(cases_file))
        .map_err(|e| format!("test/screens/{cases_file}: {e}"))?;
    let cases: Vec<Case> = parse_cases(&text)?
        .into_iter()
        .filter(|c| filters.is_empty() || filters.iter().any(|f| c.name.contains(f.as_str())))
        .collect();
    let exe = root.join(crate::dist_exe());
    let out = root.join("target/screens");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    // 4 つずつ並べて動かす（1 ケースは数秒、ほとんど待ち）
    let mut results: Vec<(String, Result<String, String>)> = Vec::new();
    for chunk in cases.chunks(4) {
        let done: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|c| s.spawn(|| (c.name.clone(), run_case(&exe, &out, c)))).collect();
            hs.into_iter().map(|h| h.join().expect("ケースのスレッドが落ちた")).collect()
        });
        results.extend(done);
    }

    let (mut ok, mut ng) = (0, 0);
    for (name, r) in results {
        let golden = dir.join(format!("{name}.txt"));
        match r {
            Err(e) => {
                ng += 1;
                eprintln!("NG {name}: {e}");
            }
            Ok(actual) if bless => {
                std::fs::write(&golden, &actual).map_err(|e| format!("{}: {e}", golden.display()))?;
                ok += 1;
                eprintln!("書いた {name}");
            }
            Ok(actual) => match std::fs::read_to_string(&golden) {
                Ok(expected) if normalize(&expected) == actual => {
                    ok += 1;
                    eprintln!("OK {name}");
                }
                Ok(expected) => {
                    ng += 1;
                    eprintln!("NG {name}（target/screens/{name}.png）\n{}", describe_diff(&normalize(&expected), &actual));
                }
                Err(_) => {
                    ng += 1;
                    eprintln!("NG {name}: 期待値 {} がありません（--bless で作る）", golden.display());
                }
            },
        }
    }
    eprintln!("画面の回帰テスト: OK {ok} / NG {ng}");
    if ng == 0 { Ok(()) } else { Err(format!("{ng} 件が期待値と違います")) }
}

/// Emacs の自動確認の 1 つ。`log` に `] NG ` か「時間切れ」があれば失敗
struct EmacsTest {
    name: &'static str,
    /// test/ の下のファイル（.el なら emacs -Q -l、.sh なら bash）
    file: &'static str,
    log: &'static str,
    /// 既定で動かすか（自分で合否を決めない・時間のかかるものは名前を指定したときだけ）
    default: bool,
}

/// macOS の試験（T-126。Windows の試験は cmd / pwsh / ConPTY を前提にしているので動かない）
#[cfg(not(windows))]
const EMACS_TESTS: &[EmacsTest] = &[
    EmacsTest { name: "smoke-mac", file: "smoke-mac.el", log: "target/t126/smoke-mac.log", default: true },
    EmacsTest { name: "features", file: "features-mac.el", log: "target/t128/features-mac.log", default: true },
    // スクロールバック（T-135）
    EmacsTest { name: "scrollback", file: "scrollback-mac.el", log: "target/t135/scrollback-mac.log", default: true },
    // 性能の測定（T-128）。時間がかかるので既定では動かさない
    EmacsTest { name: "perf", file: "perf-mac.el", log: "target/t128/perf-run.log", default: false },
    // ghostel との比べ（T-122）。利用者の ~/.emacs.d の ghostel を読む
    EmacsTest { name: "ghostel", file: "perf-ghostel.el", log: "target/t128/ghostel.log", default: false },
];

#[cfg(windows)]
const EMACS_TESTS: &[EmacsTest] = &[
    EmacsTest { name: "smoke", file: "smoke.el", log: "target/t028/smoke.log", default: true },
    EmacsTest { name: "integration", file: "integration.el", log: "target/t034/integration.log", default: true },
    EmacsTest { name: "split", file: "split.el", log: "target/t063/split.log", default: true },
    EmacsTest { name: "env", file: "env.el", log: "target/t109/env.log", default: true },
    EmacsTest { name: "resize", file: "resize.el", log: "target/t032/resize.log", default: true },
    EmacsTest { name: "font", file: "font.el", log: "target/t033/font.log", default: true },
    EmacsTest { name: "frames", file: "frames.el", log: "target/t044/frames.log", default: true },
    EmacsTest { name: "copy", file: "copy.el", log: "target/t086/copy.log", default: true },
    EmacsTest { name: "meta", file: "meta.el", log: "target/t089/meta.log", default: true },
    EmacsTest { name: "scheme", file: "scheme.el", log: "target/t102/scheme.log", default: true },
    EmacsTest { name: "idle", file: "idle.el", log: "target/t104/idle-notify.log", default: true },
    EmacsTest { name: "bigimage", file: "bigimage.el", log: "target/t105/bigimage.log", default: true },
    EmacsTest { name: "shellcmd", file: "shellcmd.el", log: "target/t118/shellcmd.log", default: true },
    EmacsTest { name: "prompt", file: "prompt.el", log: "target/t119/prompt.log", default: true },
    EmacsTest { name: "ime", file: "ime.el", log: "target/t137/ime.log", default: true },
    EmacsTest { name: "kitty", file: "kitty.el", log: "target/t088/emacs/kitty.log", default: true },
    EmacsTest { name: "close", file: "close.el", log: "target/t054/close.log", default: true },
    EmacsTest { name: "cleanup", file: "cleanup.sh", log: "target/t041/cleanup.log", default: true },
    // 送ったキーと届いたものの記録を作るだけ（見比べは人）
    EmacsTest { name: "keys", file: "keys.el", log: "target/t069/keys.log", default: false },
    // 性能の測定
    EmacsTest { name: "perf", file: "perf.el", log: "target/t054/perf-default.log", default: false },
];

/// Git for Windows の bash。`Command::new("bash")` は Windows の検索順で System32 の bash.exe（WSL）を先に見つけるので、
/// 環境変数 BASH → PATH（System32 と WindowsApps を除く）→ 既定の置き場 の順に探す
fn git_bash() -> PathBuf {
    if let Some(b) = std::env::var_os("BASH").filter(|b| Path::new(b).is_file()) {
        return b.into();
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|d| {
                let s = d.to_string_lossy().to_lowercase();
                !s.contains("system32") && !s.contains("windowsapps")
            })
            .map(|d| d.join("bash.exe"))
            .find(|p| p.is_file())
    });
    found.unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"))
}

fn emacs_exe() -> PathBuf {
    let default = if cfg!(windows) { "C:/Apps/emacs/emacs-31.1/bin/emacs.exe" } else { "emacs" };
    std::env::var_os("EMACS").map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// ログの失敗の行（`[時刻] NG 名前` と時間切れ）
fn log_failures(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| {
            let body = l.split_once("] ").map_or(*l, |(_, b)| b);
            (body.starts_with("NG ") && !body.starts_with("NG の数")) || body.contains("時間切れ")
        })
        .map(str::to_string)
        .collect()
}

pub fn test_emacs(args: &[String]) -> Result<(), String> {
    let names: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    for n in &names {
        if !EMACS_TESTS.iter().any(|t| t.name == n.as_str()) {
            let all: Vec<&str> = EMACS_TESTS.iter().map(|t| t.name).collect();
            return Err(format!("知らない試験: {n}（{}）", all.join(" ")));
        }
    }
    if !args.iter().any(|a| a == "--no-dist") {
        crate::dist()?;
    }
    let root = root();
    let emacs = emacs_exe();
    let mut failed = Vec::new();
    for t in EMACS_TESTS.iter().filter(|t| if names.is_empty() { t.default } else { names.iter().any(|n| n.as_str() == t.name) }) {
        let log = root.join(t.log);
        let _ = std::fs::remove_file(&log);
        let start = Instant::now();
        eprintln!("--- {}", t.name);
        let mut cmd = if t.file.ends_with(".sh") {
            let mut c = Command::new(git_bash());
            c.arg(format!("test/{}", t.file));
            c
        } else {
            let mut c = Command::new(&emacs);
            // GUI の Emacs では default-directory が当てにならないので、絶対パスと EMBTERM_ROOT で渡す（T-124）
            c.args(["-Q", "-l"]).arg(root.join("test").join(t.file));
            c.env("EMBTERM_ROOT", &root);
            c
        };
        cmd.current_dir(&root).stdout(Stdio::null()).stderr(Stdio::null());
        let status = wait_with_timeout(cmd, Duration::from_secs(400));
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let mut problems = log_failures(&text);
        match status {
            Ok(true) => {}
            Ok(false) => problems.push("終了コードが 0 でない".into()),
            Err(e) => problems.push(e),
        }
        if text.is_empty() {
            problems.push(format!("ログ {} が無い", log.display()));
        }
        let ok_count = text.lines().filter(|l| l.split_once("] ").is_some_and(|(_, b)| b.starts_with("OK "))).count();
        if problems.is_empty() {
            eprintln!("OK {}（{:.0} 秒、OK の行 {ok_count}）", t.name, start.elapsed().as_secs_f32());
        } else {
            eprintln!("NG {}（{}）", t.name, log.display());
            for p in &problems {
                eprintln!("  {p}");
            }
            failed.push(t.name);
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("失敗した試験: {}", failed.join(" "))) }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn parse_and_normalize() {
        let cases = parse_cases("# c\n@g 80 24 100 50 200 -- a b\nx\t1\\r\t\\r\ny settle=9\t2\\r\n").unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].inputs, vec!["1\\r", "\\r"]);
        assert_eq!(cases[0].command, vec!["a", "b"]);
        assert_eq!(cases[0].shot_at(), 100 + 50 + 200);
        assert_eq!(cases[1].settle, 9);
        assert_eq!(normalize("a  \r\nb\n\n  \n"), "a\nb\n");
        assert_eq!(log_failures("[1] OK a\n[2] NG b -- x\n[3] NG の数: 0\n[4] 時間切れで終える"), vec!["[2] NG b -- x", "[4] 時間切れで終える"]);
    }
}
