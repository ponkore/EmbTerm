# ADR 0001: VT の解釈に libghostty-vt を使う

- 日付: 2026-11-11
- ステータス: 採用（TODO [T-005](../TODO.md) / T-020。調査は [../research/libghostty-vt-build.md](../research/libghostty-vt-build.md)）

## 背景

端末エミュレータの中心は VT（エスケープシーケンス）の解釈である。EmbTerm はここを自前で書くか、
既にあるものを使うかを最初に決める必要があった。

前身の ghostel が libghostty-vt を使っており、不足（OSC を C API で拾えず自前で走査していた）が
報告されていたが、それは 1.0 より前の C API の話だった。

## 検討した案

| 案 | 評価 |
|---|---|
| 自前で書く | 量が多く、vttest に通る品質まで持っていく手間が本題（Emacs への埋め込み）を圧迫する |
| vte / alacritty_terminal などの Rust のクレートを使う | Kitty graphics・reflow・スクロールバックの扱いがそれぞれ足りない。文字幅の表も合わせ込みが要る |
| **libghostty-vt 1.0 の C ABI を静的リンクする** | 必要なものがひと通り揃っている。Zig 0.17.1 でソースからビルドでき、Rust（MSVC、CRT 静的）の exe にも Emacs module にもリンクできた |

libghostty-vt 1.0 の C ABI で揃っていたもの: PTY への書き戻し、ベル、タイトル、cwd（OSC 7）、
クリップボード（OSC 52 の読み書き）、デスクトップ通知、進捗、OSC 133、未知のシーケンス、
同期出力（render hold）、reflow、render state（dirty の行だけの反復、画像）、キー・マウスのエンコーダ、
selection、search。

## 決定

**libghostty-vt 1.0 の C ABI を静的リンクして使う。** FFI と安全なラッパは `crates/vt`（`et-vt`）に閉じ込め、
unsafe をそこから外に出さない。取得とビルドは `cargo xtask fetch-ghostty-vt` が行う
（Windows は Zig でビルド、macOS は上流の xcframework を取り出す）。

## 帰結

- VT の品質は上流に乗れる。vttest 2.7 のメニュー 1〜11 で、ふだん使う範囲は合格した（T-055）。
  落ちたのは libghostty-vt が持たない機能（DECDWL/DECDHL、VT52 の画面、矩形の操作、いくつかの報告）。
- **eaw の幅を切り替える設定は持てない**（幅の表は固定で eaw は幅 1）。これは [ADR 0006](0006-eaw-width.md) で
  「1 桁で固定」と決める根拠になった。
- 端末はスレッド安全ではないので、複数のスレッドから触るときは呼ぶ側が `Mutex` で守る。
  `Terminal` は `Send` だが `Sync` ではない、という形で型に出してある。
- ビルドに **Zig 0.17.x** が要る（Windows のみ）。版は libghostty-vt の `build.zig.zon` の
  `minimum_zig_version` と minor まで合わせる。
- ライセンスは本体が MIT。同梱される highway・simdutf・wuffs・uucode も MIT / Apache-2.0 / BSD 系で、
  配布物に表記を入れる。
