//! libghostty-vt の静的ライブラリをリンクする（spikes/vt-link と同じ）。
//! 既定は vendor/ghostty-vt（`cargo xtask fetch-ghostty-vt` の出力）。GHOSTTY_VT_PREFIX で差し替えられる。
//!
//! ライブラリの名前は OS で違う（T-124）:
//! - Windows: `lib/ghostty-vt-static.lib`（zig でソースからビルドしたもの）
//! - macOS: `lib/libghostty-vt.a`（上流の xcframework から取り出したもの。universal）
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let (file, link_name) = match target_os.as_str() {
        "windows" => ("ghostty-vt-static.lib", "ghostty-vt-static"),
        _ => ("libghostty-vt.a", "ghostty-vt"),
    };
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prefix = std::env::var("GHOSTTY_VT_PREFIX")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("vendor/ghostty-vt"));
    let lib = prefix.join("lib");
    if !lib.join(file).exists() {
        println!(
            "cargo:warning={} に {file} がありません。`cargo xtask fetch-ghostty-vt` を実行してください",
            lib.display()
        );
    }
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_PREFIX");
    println!("cargo:rerun-if-changed={}", lib.join(file).display());
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static={link_name}");
    // libghostty-vt の中の C++（highway・simdutf）が libc++ を要る
    if target_os == "macos" || target_os == "ios" {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
}
