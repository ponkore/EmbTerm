//! 開発用のタスク。
//!
//! ```text
//! cargo xtask fetch-ghostty-vt [--force]
//!     libghostty-vt のソースと依存を取得し、Zig で静的ライブラリをビルドして vendor/ghostty-vt に置く。
//!     要るもの: zig 0.17.x、curl、tar、certutil（後ろ 3 つは Windows に入っている）
//!
//! cargo xtask fetch-conpty [--force]
//!     再配布版の ConPTY（NuGet の Microsoft.Windows.Console.ConPTY）を nuget.org から取り、
//!     SHA256 と Authenticode の署名を確かめて、x64 の conpty.dll と OpenConsole.exe を vendor/conpty に置く。
//!     要るもの: curl、tar、certutil、PowerShell（どれも Windows に入っている）
//!
//! cargo xtask dist
//!     release でビルドし、target/dist/ に配布物を集める（T-028 / T-030）:
//!     embterm_module.dll、embterm.el、embterm.exe、conpty.dll、x64/OpenConsole.exe
//!     （ConPTY は vendor/conpty から。無ければ先に `cargo xtask fetch-conpty`）
//!
//! cargo xtask install [DIR]
//!     release でビルドし、配布物を DIR（既定 C:\Apps\embterm）に写す（T-057）。普段使いの Emacs はここを読む。
//!     中身が同じファイルは写さない。使用中（Emacs が読み込んだ conpty.dll、動いている OpenConsole.exe）で
//!     上書きできないものは `<名前>.old-<時刻>` に名前を変えてから写す（使用中のファイルも名前は変えられる）。
//!     新しい版が使われるのは、module なら Emacs の再起動後、ConPTY なら次に開く端末から
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// libghostty-vt の版。上げるときは下の SHA256 も
/// https://release.files.ghostty.org/libghostty-vt/<版>/SHA256SUMS から写す
const GHOSTTY_VT_VERSION: &str = "1.0.0";
const GHOSTTY_VT_SHA256: &str = "4a6ef758be721923e1969e7b81b443dcc92d8dd3fb5a39600c03a8817a753676";
const GHOSTTY_VT_DEPS_SHA256: &str = "4d5ea5e448e4c7ea8fb55ccbaa8668f2a97d44f438cee831e2b7ecd162d85c29";
/// 再配布版 ConPTY の版（docs/research/conpty.md §1.2）。上げるときは下の SHA256 も書き換える
/// （nuget.org の .nupkg を落として `certutil -hashfile <file> SHA256` で求めた値。2026-11-11 に取ったもの）
const CONPTY_VERSION: &str = "1.25.260930003";
const CONPTY_NUPKG_SHA256: &str = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee";
/// Zig の版は libghostty-vt の build.zig.zon の minimum_zig_version と minor まで一致させる
const ZIG_MINOR: &str = "0.17.";
/// `cargo xtask install` の既定の置き場（GenPreview の C:\Apps\genpreview と同じ形）
const DEFAULT_INSTALL_DIR: &str = r"C:\Apps\embterm";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().cloned().unwrap_or_default();
    let force = args.iter().any(|a| a == "--force");
    let result = match task.as_str() {
        "fetch-ghostty-vt" => fetch_ghostty_vt(force),
        "fetch-conpty" => fetch_conpty(force),
        "dist" => dist(),
        "install" => install(args.get(1).map_or_else(|| PathBuf::from(DEFAULT_INSTALL_DIR), PathBuf::from)),
        _ => {
            eprintln!(
                "使い方: cargo xtask <fetch-ghostty-vt | fetch-conpty> [--force] | cargo xtask dist | cargo xtask install [DIR]"
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

/// certutil で SHA256 を求めて照合する
fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let out = output(Command::new("certutil").arg("-hashfile").arg(file).arg("SHA256"))?;
    // 2 行目がハッシュ（古い certutil は 2 桁ごとに空白を入れる）
    let actual: String = out.lines().nth(1).unwrap_or_default().chars().filter(|c| c.is_ascii_hexdigit()).collect();
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
    if prefix.join("lib/ghostty-vt-static.lib").exists() && !force {
        eprintln!("{} は用意済み（作り直すなら --force）", prefix.display());
        return Ok(());
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

/// release でビルドし、配布物の（元のファイル, 配布物の中の相対パス）を返す
fn build_release() -> Result<Vec<(PathBuf, &'static str)>, String> {
    let root = root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run(Command::new(&cargo)
        .args(["build", "--release", "-p", "embterm", "-p", "embterm-module"])
        .current_dir(&root))?;
    let release = root.join("target/release");
    let conpty = root.join("vendor/conpty");
    let files = vec![
        (release.join("embterm_module.dll"), "embterm_module.dll"),
        (release.join("embterm.exe"), "embterm.exe"),
        (root.join("lisp/embterm.el"), "embterm.el"),
        (conpty.join("conpty.dll"), "conpty.dll"),
        (conpty.join("x64/OpenConsole.exe"), "x64/OpenConsole.exe"),
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
        std::fs::copy(src, &dst).map_err(|e| format!("{} → {}: {e}", src.display(), dst.display()))?;
        eprintln!("{}", dst.display());
    }
    eprintln!("できた: {}", dist.display());
    Ok(())
}

/// release でビルドし、配布物を DIR に写す（target/dist は経由しない）。使用中で上書きできないファイルは名前を変えて退避してから写す
fn install(dir: PathBuf) -> Result<(), String> {
    let files = build_release()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::create_dir_all(dir.join("x64")).map_err(|e| format!("{}: {e}", dir.display()))?;
    // 前に退避したもの。もう使われていなければ消せる（使用中なら残す）
    for sub in [dir.clone(), dir.join("x64")] {
        if let Ok(entries) = std::fs::read_dir(&sub) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().contains(".old-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    for (src, name) in &files {
        let dst = dir.join(name);
        let new = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
        if std::fs::read(&dst).is_ok_and(|old| old == new) {
            eprintln!("同じ: {}", dst.display());
            continue;
        }
        if std::fs::write(&dst, &new).is_err() {
            // 使用中（読み込まれた DLL・動いている exe）は書けないが、名前は変えられる
            let moved = dst.with_file_name(format!("{}.old-{stamp}", dst.file_name().unwrap().to_string_lossy()));
            std::fs::rename(&dst, &moved).map_err(|e| format!("{} を退避できない: {e}", dst.display()))?;
            std::fs::write(&dst, &new).map_err(|e| format!("{}: {e}", dst.display()))?;
            eprintln!("入れ替え（使用中のものは {} に退避）: {}", moved.display(), dst.display());
        } else {
            eprintln!("写した: {}", dst.display());
        }
    }
    eprintln!("できた: {}（module の新しい版は Emacs の再起動後に使われる）", dir.display());
    Ok(())
}
