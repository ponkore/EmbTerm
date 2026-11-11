# libghostty-vt を Windows でビルドし、Rust から呼ぶ

- 日付: 2026-11-11
- TODO T-020 の記録（T-005 の材料）
- スパイク: `spikes/vt-link`（exe）、`spikes/vt-module`（Emacs module）

## 結論

**使える。** Zig 0.17.1 でソースからビルドした静的ライブラリを、Rust（`x86_64-pc-windows-msvc`、
CRT の静的リンク）の exe にも cdylib（Emacs module）にもそのままリンクでき、Emacs 31.1 の
公式ビルド（`x86_64-w64-mingw32`）に読み込めた。追加の DLL は要らない。

| 確かめたこと | 結果 |
|---|---|
| Zig 0.17.1（scoop）で `zig build` | **通った**。キャッシュが空の状態から 3 分 40 秒 |
| 静的ライブラリを Rust の exe にリンク（debug / release + LTO） | **通った**。exe は 2.5 MB |
| 同じく cdylib（emacs クレート 0.23）にリンクし、`emacs -Q --batch` で `module-load` | **通った**。DLL は 2.6 MB |
| できた exe / DLL が読み込む DLL | KERNEL32、ntdll、api-ms-win-core-synch（DLL はさらに bcryptprimitives）。**VC++ の再頒布パッケージは要らない** |
| `ghostty_version()` | `1.0.0` |

## 手順

```sh
# ソースと依存の tarball（同じ版どうし）。SHA256SUMS で確かめる
curl -LO https://release.files.ghostty.org/libghostty-vt/1.0.0/libghostty-vt-1.0.0.tar.gz
curl -LO https://release.files.ghostty.org/libghostty-vt/1.0.0/libghostty-vt-1.0.0-deps.tar.gz
tar xzf libghostty-vt-1.0.0.tar.gz && tar xzf libghostty-vt-1.0.0-deps.tar.gz
cd libghostty-vt-1.0.0
# ZIG_GLOBAL_CACHE_DIR は新しい空のディレクトリにする（PACKAGING-LIBGHOSTTY.md の注意）
ZIG_GLOBAL_CACHE_DIR=../zig-cache-gnu zig build --prefix ../out-gnu \
  --system ../libghostty-vt-1.0.0-deps -Doptimize=ReleaseFast -Dcpu=baseline
```

**この手順は `cargo xtask fetch-ghostty-vt` にした**（T-064）。tarball の取得と SHA256 の照合
（`certutil`）、展開、`zig build`（`-Dtarget=x86_64-windows-msvc` を明示）を行い、`vendor/ghostty-vt` に置く。
キャッシュは `vendor/zig-cache`。`vendor/` は `.gitignore` 済み。

（スパイクの最初の試行では `--prefix ../out-gnu`、キャッシュ `../zig-cache-gnu` で手でビルドした。
`vendor/out-gnu` と `vendor/zig-cache-gnu` はもう使っていないので消してよい。）

出力（`--prefix` の下）:

| ファイル | 中身 |
|---|---|
| `lib/ghostty-vt-static.lib` | 静的ライブラリ（13 MB。COFF の obj が 12 個。highway・simdutf・wuffs・compiler_rt を含む） |
| `bin/ghostty-vt.dll` + `lib/ghostty-vt.lib` | DLL と import library（使わない） |
| `include/ghostty/vt.h`、`include/ghostty/vt/*.h` | ヘッダ。静的リンクでは `GHOSTTY_STATIC` を定義して読む |

`-Dtarget` を付けないと、Zig はこのマシンでは **MSVC の ABI** でビルドした（DLL が
`VCRUNTIME140.dll` と UCRT を読み込むことから分かる）。静的ライブラリは CRT 静的の
Rust にリンクしても CRT の DLL を引き込まなかった。

`-Dtarget=x86_64-windows-msvc` を明示しても同じ結果になった（DLL の依存も同じ、3 分 24 秒）。
`-W linker-messages` を付けてリンカの出力を表示させても、**警告は出なかった**（`/MD` と `/MT` の混在の警告も無し）。

## 文字幅（T-011 / T-012 の材料）

`ghostty_unicode_codepoint_width()` と、端末に書いたあとのカーソルの桁で確かめた。

| 文字 | 幅 |
|---|---|
| `a` | 1 |
| `あ` `色` | 2 |
| `①` U+2460、`★` U+2605、`※` U+203B、`─` U+2500、`│` U+2502、`○` U+25CB、`…` U+2026 | **1** |
| `😀` U+1F600 | 2 |
| `` U+E0B0（Powerline、私用領域） | 1 |

- **East Asian Ambiguous は幅 1。** claude の数え方（`①` を 1 桁）と一致する
- **幅を切り替える設定は C API に無い。** `unicode.h` の幅表は固定で、端末も同じ表を使う。
  requirements.md §5.5 の「eaw を 2 桁として数える設定も選べる」は、libghostty-vt に手を入れない限り
  できない（T-012 で扱う）
- 書記素クラスタの幅（絵文字の VS16 など）は mode 2027 が有効なときの規則に従う（`ghostty_unicode_grapheme_width`）

## Rust からの呼び方（今後のため）

- スパイクでは関数の宣言を手で書いた。enum は `int`（`GHOSTTY_ENUM_TYPED`）、ハンドルは不透明なポインタ、
  結果は `GhosttyResult`（0 = 成功、負 = 失敗）
- 本番は bindgen で生成するか、使う範囲だけ手で書くかを決める（bindgen には libclang が要るが、
  このマシンには入っていない）→ T-065
- API は `ghostty_terminal_*`（端末）、`ghostty_render_state_*`（描画用のスナップショット。行・セルの
  反復、dirty の行だけの反復、画像の反復）、キー・マウスのエンコーダ、selection、search、formatter など。
  何をどう使うかは ghostel の使い方（T-019）と合わせて決める
