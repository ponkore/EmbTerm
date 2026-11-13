//! 開発用のタスク。
//!
//! ```text
//! cargo xtask fetch-ghostty-vt [--force]
//!     libghostty-vt の静的ライブラリを vendor/ghostty-vt に置く。取り方は OS で違う（T-124）:
//!     - Windows: ソースと依存を取り、Zig でビルドする。要るもの: zig 0.17.x、curl、tar、certutil
//!     - macOS: 上流の xcframework（universal の libghostty-vt.a + ヘッダ）を取り出す。**zig は要らない**。
//!       要るもの: curl、unzip、shasum
//!
//! cargo xtask fetch-conpty [--force]（Windows だけ）
//!     再配布版の ConPTY（NuGet の Microsoft.Windows.Console.ConPTY）を nuget.org から取り、
//!     SHA256 と Authenticode の署名を確かめて、x64 の conpty.dll と OpenConsole.exe を vendor/conpty に置く。
//!     要るもの: curl、tar、certutil、PowerShell（どれも Windows に入っている）
//!
//! cargo xtask dist
//!     release でビルドし、target/dist/ に配布物を集める（T-028 / T-030）:
//!     embterm_module.dll、embterm.el、embterm.exe、conpty.dll、x64/OpenConsole.exe、etc/embterm.{ps1,bash,lua}（シェル統合）
//!     macOS では（T-124 の段階では）ヘッドレスの embterm と embterm.el、etc/embterm.bash だけ
//!     （ConPTY は vendor/conpty から。無ければ先に `cargo xtask fetch-conpty`）。
//!     普段使いの Emacs へは、Emacs の側（~/.emacs.d の `my:embterm-update`）が target/dist を写して入れる（T-120）。
//!     **普段の Emacs に target/dist を直接読ませない**（conpty.dll と OpenConsole.exe がロックされ、dist と試験が上書きに失敗する）
//!
//! cargo xtask test-screens [--bless] [--no-dist] [名前の一部...]
//!     画面の回帰テスト（T-078 / T-114）。test/screens/cases.txt のケースを embterm.exe で動かし、画面のテキストを
//!     test/screens/<名前>.txt と比べる。--bless で期待値を書き直す。vttest のケースは WSL に vttest が要る。
//!     ケースの一覧は OS で違う（Windows: cases.txt、Unix: cases-unix.txt）
//!
//! cargo xtask test-emacs [--no-dist] [名前...]
//!     Emacs の自動確認（T-081）。別に起こした `emacs -Q` で test/*.el を順に動かし、終了コードとログの NG を見る。
//!     名前を省くと合否を自分で決めるものすべて。Emacs は環境変数 EMACS（既定 C:/Apps/emacs/emacs-31.1/bin/emacs.exe）
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod tests;

/// libghostty-vt の版。上げるときは下の SHA256 も
/// https://release.files.ghostty.org/libghostty-vt/<版>/SHA256SUMS から写す
const GHOSTTY_VT_VERSION: &str = "1.0.0";
const GHOSTTY_VT_SHA256: &str = "4a6ef758be721923e1969e7b81b443dcc92d8dd3fb5a39600c03a8817a753676";
const GHOSTTY_VT_DEPS_SHA256: &str = "4d5ea5e448e4c7ea8fb55ccbaa8668f2a97d44f438cee831e2b7ecd162d85c29";
/// Apple 向けの配布物（macOS / iOS の静的ライブラリとヘッダ。macOS では zig でビルドせずこれを使う。T-124）
const GHOSTTY_VT_XCFRAMEWORK_SHA256: &str = "2c32b7409b2b6c2ef6cfd606526de357762ad458bdc27134ad074e86506a8bc2";
/// 再配布版 ConPTY の版（docs/research/conpty.md §1.2）。上げるときは下の SHA256 も書き換える
/// （nuget.org の .nupkg を落として `certutil -hashfile <file> SHA256` で求めた値。2026-11-11 に取ったもの）
const CONPTY_VERSION: &str = "1.25.260930003";
const CONPTY_NUPKG_SHA256: &str = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee";
/// Zig の版は libghostty-vt の build.zig.zon の minimum_zig_version と minor まで一致させる
const ZIG_MINOR: &str = "0.17.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().cloned().unwrap_or_default();
    let force = args.iter().any(|a| a == "--force");
    let result = match task.as_str() {
        "fetch-ghostty-vt" => fetch_ghostty_vt(force),
        "fetch-conpty" => fetch_conpty(force),
        "dist" => dist(),
        "test-screens" => tests::test_screens(&args[1..]),
        "test-emacs" => tests::test_emacs(&args[1..]),
        _ => {
            eprintln!(
                "使い方: cargo xtask <fetch-ghostty-vt | fetch-conpty> [--force] | dist | test-screens [--bless] [--no-dist] [名前...] | test-emacs [--no-dist] [名前...]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn run(cmd: &mut Command) -> Result<(), String> {
    eprintln!("> {cmd:?}");
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{cmd:?} が失敗しました（{status}）"))
}

fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{cmd:?} が失敗しました（{}）", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// SHA256 を求めて照合する（Windows は certutil、Unix は shasum）
fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let actual: String = if cfg!(windows) {
        let out = output(Command::new("certutil").arg("-hashfile").arg(file).arg("SHA256"))?;
        // 2 行目がハッシュ（古い certutil は 2 桁ごとに空白を入れる）
        out.lines().nth(1).unwrap_or_default().chars().filter(|c| c.is_ascii_hexdigit()).collect()
    } else {
        let out = output(Command::new("shasum").args(["-a", "256"]).arg(file))?;
        out.split_whitespace().next().unwrap_or_default().to_string()
    };
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!("{} の SHA256 が一致しません\n  期待: {expected}\n  実際: {actual}", file.display()))
    }
}

fn download(url: &str, dest: &Path, sha256: &str) -> Result<(), String> {
    if dest.exists() && verify_sha256(dest, sha256).is_ok() {
        return Ok(());
    }
    run(Command::new("curl").args(["-sSfL", "-o"]).arg(dest).arg(url))?;
    verify_sha256(dest, sha256)
}

fn fetch_ghostty_vt(force: bool) -> Result<(), String> {
    let vendor = root().join("vendor");
    let prefix = vendor.join("ghostty-vt");
    let lib = prefix.join(if cfg!(windows) { "lib/ghostty-vt-static.lib" } else { "lib/libghostty-vt.a" });
    if lib.exists() && !force {
        eprintln!("{} は用意済み（作り直すなら --force）", prefix.display());
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        return fetch_ghostty_vt_apple(&vendor, &prefix);
    }
    if !cfg!(windows) {
        return Err("この OS の取り方は決めていません（Windows は zig でビルド、macOS は xcframework）".into());
    }

    let zig = output(Command::new("zig").arg("version")).map_err(|e| format!("zig が見つかりません: {e}"))?;
    if !zig.trim().starts_with(ZIG_MINOR) {
        return Err(format!("zig {ZIG_MINOR}x が要ります（いまは {}）", zig.trim()));
    }

    let v = GHOSTTY_VT_VERSION;
    let dl = vendor.join("dl");
    std::fs::create_dir_all(&dl).map_err(|e| e.to_string())?;
    let base = format!("https://release.files.ghostty.org/libghostty-vt/{v}");
    let src_tgz = dl.join(format!("libghostty-vt-{v}.tar.gz"));
    let deps_tgz = dl.join(format!("libghostty-vt-{v}-deps.tar.gz"));
    download(&format!("{base}/libghostty-vt-{v}.tar.gz"), &src_tgz, GHOSTTY_VT_SHA256)?;
    download(&format!("{base}/libghostty-vt-{v}-deps.tar.gz"), &deps_tgz, GHOSTTY_VT_DEPS_SHA256)?;

    let src = vendor.join(format!("libghostty-vt-{v}"));
    let deps = vendor.join(format!("libghostty-vt-{v}-deps"));
    for (tgz, dir) in [(&src_tgz, &src), (&deps_tgz, &deps)] {
        if !dir.exists() {
            run(Command::new("tar").arg("-xzf").arg(tgz).arg("-C").arg(&vendor))?;
        }
    }

    // キャッシュは --system の依存だけで埋まる専用のもの（PACKAGING-LIBGHOSTTY.md の注意）。
    // 空から作ると 4 分ほどかかる
    run(Command::new("zig")
        .arg("build")
        .arg("--prefix")
        .arg(&prefix)
        .arg("--system")
        .arg(&deps)
        .args(["-Doptimize=ReleaseFast", "-Dcpu=baseline", "-Dtarget=x86_64-windows-msvc"])
        .env("ZIG_GLOBAL_CACHE_DIR", vendor.join("zig-cache"))
        .current_dir(&src))?;

    eprintln!("できた: {}", prefix.join("lib/ghostty-vt-static.lib").display());
    Ok(())
}

/// T-124: macOS では上流の xcframework（universal の静的ライブラリ + ヘッダ）を取り出す。zig は要らない。
///
/// 中身は `ghostty-vt.xcframework/macos-arm64_x86_64/{libghostty-vt.a, Headers/}`（ほかに iOS 向け）。
/// zig でビルドした Windows 版と同じ配置（`lib/` と `include/`）に並べ直す
fn fetch_ghostty_vt_apple(vendor: &Path, prefix: &Path) -> Result<(), String> {
    let v = GHOSTTY_VT_VERSION;
    let dl = vendor.join("dl");
    std::fs::create_dir_all(&dl).map_err(|e| e.to_string())?;
    let zip = dl.join(format!("ghostty-vt-{v}.xcframework.zip"));
    download(
        &format!("https://release.files.ghostty.org/libghostty-vt/{v}/ghostty-vt-{v}.xcframework.zip"),
        &zip,
        GHOSTTY_VT_XCFRAMEWORK_SHA256,
    )?;

    // 展開は毎回新しい空のディレクトリに行う（fetch-conpty と同じ作法）
    let extract = dl.join(format!("ghostty-vt-{v}-xcframework"));
    if extract.exists() {
        std::fs::remove_dir_all(&extract).map_err(|e| format!("{}: {e}", extract.display()))?;
    }
    std::fs::create_dir_all(&extract).map_err(|e| e.to_string())?;
    run(Command::new("unzip").arg("-q").arg(&zip).arg("-d").arg(&extract))?;

    let slice = extract.join("ghostty-vt.xcframework/macos-arm64_x86_64");
    let src_lib = slice.join("libghostty-vt.a");
    let src_include = slice.join("Headers");
    if !src_lib.exists() || !src_include.exists() {
        return Err(format!("{} に libghostty-vt.a と Headers がありません（中の配置が変わった？）", slice.display()));
    }

    let lib_dir = prefix.join("lib");
    let include_dir = prefix.join("include");
    std::fs::create_dir_all(&lib_dir).map_err(|e| e.to_string())?;
    if include_dir.exists() {
        std::fs::remove_dir_all(&include_dir).map_err(|e| format!("{}: {e}", include_dir.display()))?;
    }
    let dst_lib = lib_dir.join("libghostty-vt.a");
    std::fs::copy(&src_lib, &dst_lib).map_err(|e| format!("{} へのコピー: {e}", dst_lib.display()))?;
    copy_dir(&src_include, &include_dir)?;
    std::fs::write(prefix.join("VERSION.txt"), format!("libghostty-vt {v} (xcframework macos-arm64_x86_64)\n"))
        .map_err(|e| e.to_string())?;
    std::fs::remove_dir_all(&extract).ok();
    eprintln!("できた: {}", dst_lib.display());
    Ok(())
}

/// ディレクトリを丸ごとコピーする（ヘッダの取り出し用）
fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Windows に付いている tar（bsdtar）。zip 形式の .nupkg も展開できる。
/// Git for Windows の tar（GNU tar）が PATH の先にあっても取り違えないよう、フルパスで呼ぶ
fn system_tar() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join("tar.exe")
}

/// PowerShell の Get-AuthenticodeSignature で、署名が有効で署名者が Microsoft であることを確かめる
fn verify_authenticode(file: &Path) -> Result<(), String> {
    let script = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; Write-Output $s.Status; Write-Output $s.SignerCertificate.Subject",
        file.display().to_string().replace('\'', "''")
    );
    let out = output(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]))?;
    let mut lines = out.lines();
    let status = lines.next().unwrap_or_default().trim().to_string();
    let subject = lines.next().unwrap_or_default().trim().to_string();
    if status == "Valid" && subject.contains("O=Microsoft Corporation") {
        eprintln!("署名 OK: {} ({subject})", file.display());
        Ok(())
    } else {
        Err(format!("{} の署名を確かめられません（Status = {status}, Subject = {subject}）", file.display()))
    }
}

/// T-072: 再配布版の ConPTY を取る。
///
/// .nupkg の中身は `runtimes/win-x64/native/conpty.dll` と `build/native/runtimes/x64/OpenConsole.exe`。
/// conpty.dll は自分の隣か `x64\` の下の OpenConsole.exe を使い、どちらにも無いと黙って OS 同梱の conhost を
/// 使う（docs/research/conpty.md §1.3）ので、`vendor/conpty/conpty.dll` と `vendor/conpty/x64/OpenConsole.exe` に並べ直す
fn fetch_conpty(force: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("fetch-conpty は Windows だけです（ConPTY は Windows の仕組み）".into());
    }
    let vendor = root().join("vendor");
    let dest = vendor.join("conpty");
    let dll = dest.join("conpty.dll");
    let exe = dest.join("x64").join("OpenConsole.exe");
    if dll.exists() && exe.exists() && !force {
        eprintln!("{} は用意済み（作り直すなら --force）", dest.display());
        return Ok(());
    }

    let v = CONPTY_VERSION;
    let dl = vendor.join("dl");
    std::fs::create_dir_all(&dl).map_err(|e| e.to_string())?;
    let nupkg = dl.join(format!("microsoft.windows.console.conpty.{v}.nupkg"));
    download(
        &format!(
            "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/{v}/microsoft.windows.console.conpty.{v}.nupkg"
        ),
        &nupkg,
        CONPTY_NUPKG_SHA256,
    )?;

    // 展開は毎回新しい空のディレクトリに行う
    let extract = dl.join(format!("conpty-{v}"));
    if extract.exists() {
        std::fs::remove_dir_all(&extract).map_err(|e| format!("{}: {e}", extract.display()))?;
    }
    std::fs::create_dir_all(&extract).map_err(|e| e.to_string())?;
    run(Command::new(system_tar()).arg("-xf").arg(&nupkg).arg("-C").arg(&extract))?;

    let src_dll = extract.join("runtimes/win-x64/native/conpty.dll");
    let src_exe = extract.join("build/native/runtimes/x64/OpenConsole.exe");
    for f in [&src_dll, &src_exe] {
        if !f.exists() {
            return Err(format!("パッケージに {} がありません（中の配置が変わった？）", f.display()));
        }
        verify_authenticode(f)?;
    }

    std::fs::create_dir_all(dest.join("x64")).map_err(|e| e.to_string())?;
    std::fs::copy(&src_dll, &dll).map_err(|e| format!("{} へのコピー: {e}", dll.display()))?;
    std::fs::copy(&src_exe, &exe).map_err(|e| format!("{} へのコピー: {e}", exe.display()))?;
    std::fs::write(dest.join("VERSION.txt"), format!("Microsoft.Windows.Console.ConPTY {v}\n"))
        .map_err(|e| e.to_string())?;
    std::fs::remove_dir_all(&extract).ok();
    eprintln!("できた: {} と {}", dll.display(), exe.display());
    Ok(())
}

/// 配布物の中の embterm（デバッグ用の実行ファイル）の場所
pub fn dist_exe() -> &'static str {
    if cfg!(windows) { "target/dist/embterm.exe" } else { "target/dist/embterm" }
}

/// release でビルドし、配布物の（元のファイル, 配布物の中の相対パス）を返す
fn build_release() -> Result<Vec<(PathBuf, &'static str)>, String> {
    let root = root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let packages: &[&str] = &["embterm", "embterm-module"];
    let mut build = Command::new(&cargo);
    build.args(["build", "--release"]);
    for p in packages {
        build.args(["-p", p]);
    }
    run(build.current_dir(&root))?;
    let release = root.join("target/release");
    if !cfg!(windows) {
        // macOS の配布物（T-126）: module（.dylib）・embterm・embterm.el・シェル統合（bash）
        return Ok(vec![
            (release.join("libembterm_module.dylib"), "embterm_module.dylib"),
            (release.join("embterm"), "embterm"),
            (root.join("lisp/embterm.el"), "embterm.el"),
            (root.join("etc/embterm.bash"), "etc/embterm.bash"),
        ]);
    }
    let conpty = root.join("vendor/conpty");
    let files = vec![
        (release.join("embterm_module.dll"), "embterm_module.dll"),
        (release.join("embterm.exe"), "embterm.exe"),
        (root.join("lisp/embterm.el"), "embterm.el"),
        (conpty.join("conpty.dll"), "conpty.dll"),
        (conpty.join("x64/OpenConsole.exe"), "x64/OpenConsole.exe"),
        // シェル統合（T-118 / T-119）
        (root.join("etc/embterm.ps1"), "etc/embterm.ps1"),
        (root.join("etc/embterm.bash"), "etc/embterm.bash"),
        (root.join("etc/embterm.lua"), "etc/embterm.lua"),
    ];
    for (src, _) in &files {
        if !src.exists() {
            return Err(format!("{} がありません（ConPTY なら `cargo xtask fetch-conpty`）", src.display()));
        }
    }
    Ok(files)
}

/// release でビルドし、target/dist/ に配布物を集める。embterm.el は自分の隣の module と conpty.dll を使う
fn dist() -> Result<(), String> {
    let files = build_release()?;
    let dist = root().join("target/dist");
    std::fs::create_dir_all(dist.join("x64")).map_err(|e| e.to_string())?;
    for (src, name) in &files {
        let dst = dist.join(name);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(src, &dst).map_err(|e| format!("{} → {}: {e}", src.display(), dst.display()))?;
        eprintln!("{}", dst.display());
    }
    eprintln!("できた: {}", dist.display());
    Ok(())
}
