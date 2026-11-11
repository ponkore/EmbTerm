//! 開発用のタスク。
//!
//! ```text
//! cargo xtask fetch-ghostty-vt [--force]
//!     libghostty-vt のソースと依存を取得し、Zig で静的ライブラリをビルドして vendor/ghostty-vt に置く。
//!     要るもの: zig 0.17.x、curl、tar、certutil（後ろ 3 つは Windows に入っている）
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// libghostty-vt の版。上げるときは下の SHA256 も
/// https://release.files.ghostty.org/libghostty-vt/<版>/SHA256SUMS から写す
const GHOSTTY_VT_VERSION: &str = "1.0.0";
const GHOSTTY_VT_SHA256: &str = "4a6ef758be721923e1969e7b81b443dcc92d8dd3fb5a39600c03a8817a753676";
const GHOSTTY_VT_DEPS_SHA256: &str = "4d5ea5e448e4c7ea8fb55ccbaa8668f2a97d44f438cee831e2b7ecd162d85c29";
/// Zig の版は libghostty-vt の build.zig.zon の minimum_zig_version と minor まで一致させる
const ZIG_MINOR: &str = "0.17.";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let task = args.next().unwrap_or_default();
    let force = args.any(|a| a == "--force");
    let result = match task.as_str() {
        "fetch-ghostty-vt" => fetch_ghostty_vt(force),
        _ => {
            eprintln!("使い方: cargo xtask <fetch-ghostty-vt [--force]>");
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
