# T-088 Kitty graphics の描画 / T-052 yazi で確かめる

- 日付: 2026-11-11
- **TODO T-088（Kitty graphics の描画）と T-052（yazi）の記録**
- 結論: **libghostty-vt の render state が配置（Unicode placeholder・相対配置・切り取り・層）を解決してくれるので、自前の走査はしない。**
  et-vt は一覧と画素を読むだけ、et-render は Direct2D のビットマップにしてセルの位置へ描く（DPI・ペインの切り取り・遡ったペイン・overscan）。
  PNG は Windows の WIC でデコードする（外部クレートは増やさない）。yazi 26.9.1 は `Brand: Ghostty` / `Drivers.matches: Kgp` / `kgp: true` / `csi_16t: (8, 18)` になり、
  画像のプレビュー・切り替え・消える・終了後の画面の戻り・マウスまで embterm.exe と `emacs -Q` の中で確かめた

---

## 1. 作ったもの

| 場所 | 変更 |
|---|---|
| `crates/vt/src/ffi.rs` | 画像の FFI（`ghostty_render_state_image_*`、`ghostty_kitty_graphics_image*`、`ghostty_sys_set`、`ghostty_alloc`、`GhosttySysImage`、列挙）。`tests.rs` に `GhosttySysImage` の配置と列挙の照合（`image_enums_match_type_json`）を足した |
| `crates/vt/src/lib.rs` | `RenderState::images()`（層・z の順の一覧 `ImagePlacement`。Kitty 以外の種類は読み飛ばす）、`Terminal::kitty_image(id)`（`KittyImage`: 画素は端末の中を指す）、`Terminal::cell_px()`、`set_kitty_image_storage_limit`、`set_kitty_medium_file` |
| `crates/vt/src/png.rs`（新規） | `install_png_decoder()`。**PNG のデコーダは WIC（`GHOSTTY_SYS_OPT_DECODE_PNG` のコールバック）。** `CreateDecoder` で PNG に決めて読み（形式の自動判別はしない）、RGBA に変換して libghostty-vt の allocator の領域へ入れる。PTY の読みのスレッドから呼ばれるので呼ぶたびに `CoInitializeEx` する。Windows の wuffs 同梱は libghostty-vt の外なので使わない（文書のとおり「デコーダは持ち込み」） |
| `crates/session/src/lib.rs` | `KITTY_IMAGE_STORAGE_LIMIT`（128 MiB。libghostty-vt の既定の 10 MB は 1 枚で埋まる）、`t=f` を許す |
| `crates/render/src/frame.rs` | `Frame::images`（`PlacedImage`: 配置 + `Arc<ImagePixels>`）、`image_cell`（配置を計算したセルの大きさ）、`Frame::capture_images`（**ロックの中で呼ぶ**。画素は世代が同じあいだ写し直さない）、`PLACEHOLDER` |
| `crates/render/src/lib.rs` | `draw_rows` に分けた（`RowPass`）。**画像が無いときは今までどおり行ごと**に背景と文字を描く。画像があるときは 背景色より下の画像 → 背景色 → 文字より下の画像 → 塗りつぶしのカーソル → 文字 → 文字より上の画像 → カーソル の順に描く。placeholder のセル（`U+10EEEE`）は文字も下線も描かない |
| `crates/render/src/images.rs`（新規） | `draw_images`（`view` の行へ移し、ビューの桁・行に切り取り）、GPU のビットマップのキャッシュ（`bitmaps`: 世代が鍵、使わなくなったら `prune_images`、デバイスを失ったら捨てる）、`to_premultiplied_bgra`（RGB / RGBA / 灰色）。Direct2D の上限より大きい画像は描かない（libghostty-vt の上限 10000 は FL11 の 16384 より小さい） |
| `apps/embterm/src/{main,window}.rs` | 起動時にデコーダを登録、`refresh` のロックの中で `capture_images` |
| `apps/embterm-module/src/term.rs` | `spawn` でデコーダを登録（Lisp のスレッド、端末のスレッドより前）。`refresh` のロックの中で、いちばん下を追うペイン（`master`）と遡っているペイン（`ScrolledFrame`）の両方に `capture_images` |
| `test/kitty-graphics.py` | Kitty graphics を直接送るシーン（`basic` / `placeholder` / `delete` / `source_z` / `scroll`） |
| `test/kitty.el` | `emacs -Q` の中で yazi・split・遡るペイン・overscan を PNG に撮る（kitty.log、`target/t088/emacs/`） |

### 1.1 主な判断

- **自前の placeholder の走査はしない。** `guide-images` のとおり、render state の一覧には placeholder（1 行ごとの配置）も相対配置も入っている。`GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR` で読むだけ
- **一覧は毎回読んで持っているものと比べる。** 配置の世代（`IMAGE_PLACEMENTS_GENERATION`）は同じ render state の間でしか比べられず、遡るペインは毎回新しい render state、hold（同期出力）では render state が替わる。一覧は短いので比べるほうが単純（配置の世代は使っていない）
- 画素は render state に無いので、**端末のロックの中で `capture_images` が写す**（`Arc`。同じ世代なら写し直さない）。同期出力の hold の間は hold の始まりの render state の一覧を読み、画素はそのときの端末から世代が合うものだけ写す（端末が先に進んで消えたものは描かない）。hold が解ければ次の `update` で揃う
- セルの画素は `Terminal::cell_px`（`resize` の値）を `Frame::image_cell` に持ち、ペインのセルの大きさと比べて拡大縮小する（DPI の違うペインの見た目が揃う）
- `t=s`（共有メモリ）は libghostty-vt の既定（無効）のまま。問い合わせは失敗を返し、yazi は `kgp_shm: false` で base64（`t=d`）を使う（`ya env` で確認）
- `t=t`（一時ファイル）も既定（無効）のまま。libghostty-vt のソース（`graphics_image.zig`）では `temporary_file = .disabled` が既定で、有効にするときはディレクトリを渡す。読んだあと**ファイルを消す**動作があるので、当面は有効にしない
- `t=f`（ファイル）は許す（ghostel は既定で無効にしている。リモートのアプリが手元のファイルを読ませられる点は同じ）。Windows の危険なパスは libghostty-vt（`kitty_windows.checkPath`）が拒む

## 2. 確かめたこと

`cargo build`・`cargo test --workspace`（画像は et-vt 5 件 + et-render 2 件）・`cargo clippy --workspace --all-targets` は警告なし、`embterm.el` の byte-compile も警告なし。
test/smoke.el・test/split.el・test/keys.el は完走（NG 0）。

| 確かめたこと | 結果 | 画面 |
|---|---|---|
| `a=T,f=100,t=d`（PNG）・`f=24`・`f=32`（透明あり）・チャンク（`m=1`） | 3 枚とも位置と大きさが合う | `target/t088/kgp-basic.png` |
| `U=1` + `U+10EEEE`（placeholder。8 桁 x 4 行、中央寄せ） | 文字は描かれず画像だけ出る | `target/t088/kgp-placeholder.png` |
| `a=d,d=I` | 消えた 1 枚だけ消える | `target/t088/kgp-delete.png` |
| 切り取り（`x,y,w,h`）と拡大、`z=-1`（文字の下）と `z=1`（文字の上） | 文字が画像の上 / 下で順が合う | `target/t088/kgp-source_z.png` |
| スクロールバックを遡る（`--scroll-up`。画像の上がビューの上端で切れる） | 切れた位置に合う | `target/t088/kgp-scroll-up.png` |
| yazi 26.9.1 / embterm.exe: `ya env` | `Brand.from_env: WezTerm`（親の WezTerm の環境変数）だが、問い合わせ（XTVERSION）で `brand: Ghostty`、`kgp: true`、`kgp_shm: false`、`csi_16t: (8, 18)`、`Drivers.matches: Kgp`（`target/t088/ya-env.txt`） | `target/t088/ya-env.png` |
| yazi: PNG のプレビュー（起動 → 選択 → `j` で切り替え → テキストファイルで画像が消える） | 切り替わる / 消える | `target/t088/yazi-1-first.png` / `yazi-2-j.png` / `yazi-4-jjj.png` |
| yazi: `q` で終える（画面が元の cmd に戻る） | 戻る | `target/t088/yazi-5-q.txt` |
| yazi: マウス（`test/post-mouse.ps1` の要領でクリック・ホイール） | クリックで c-wide.png を選択しプレビューが変わる、ホイールで 2 つ下がる | `target/t088/yazi-mouse-click.png`、`yazi-mouse-wheel.txt` |
| `emacs -Q` の中で yazi | プレビューが出る | `target/t088/emacs/frame-yazi.png` |
| split（左右に分割しても崩れない） | 崩れない | `target/t088/emacs/frame-yazi-split-right.png` |
| 幅の狭いペイン（端末より狭い。ビューの右で切り取り） | 切り取られる | `target/t088/emacs/frame-basic-narrow.png` |
| 上下に分割、端末より行の多いペイン（overscan） | 大きいペインで画像が正しい行に出る | `target/t088/emacs/frame-overscan.png` |
| 遡ったペイン（上のペインだけスクロールバックへ） | 画像が遡った位置に出る | `target/t088/emacs/frame-scrolled.png` |

`emacs -Q` の確認は `test/kitty.el`（`target/t088/emacs/kitty.log`）。`yazi` の設定は `YAZI_CONFIG_HOME` を一時ディレクトリへ（ユーザーの設定は触らない）。テスト画像は `test/kitty-graphics.py` の中で作る。

## 3. 未解決・見ていないこと

- **DPI 96 以外は未確認**（ペインのセルが違うときの拡大縮小のコードはある。T-043）。デバイスを失ったときのビットマップの作り直しは機械的に起こしていない（`recreate_device` / `adopt_gpu` でキャッシュを捨てる）
- **アニメーション（`a=f` / `a=a`）は再生しない**（最初のフレームが出る）。再生するには `ghostty_kitty_graphics_animation_tick` を描画のたびに呼び、次の時刻にタイマーを掛ける
- `t=t` は無効のまま（kitten icat などの一時ファイルの経路）
- PNG のデコードは端末のロックの中（PTY の読みのスレッド。`vt_write` の中）。とても大きい画像では UI が待つ。T-054 で測る
- 選択・コピーのテキストに placeholder（`U+10EEEE` + 結合文字）が入る（`--screenshot` の txt にも見える）
- 実行中に embterm.exe のウィンドウが一瞬画面いっぱい（`リサイズ: 319x75`）になって戻るログが 2 回あった。画像と無関係にウィンドウの大きさが変わっている（WM の動作か T-043 / リサイズの調べの対象）

## 4. TODO に足すべき課題

- Kitty graphics のアニメーション（`animation_tick` と次の時刻のタイマー）
- 画像の保存量の上限の設定（`embterm` の defcustom）と `t=t` / `t=s`（`OpenFileMappingW`）の対応。yazi の `t=s` は大きな画像で base64 より速い
- 画像を含む行のコピーで placeholder の文字を除く（libghostty の formatter の扱いを確かめる）
- 親のターミナルの環境変数（`WEZTERM_*`、`TERM_PROGRAM_VERSION`）が子に伝わり、yazi の `Brand.from_env` が `WezTerm` になる（XTVERSION で上書きされるので害は無いが、`et_pty::terminal_env` で落とすか決める）
- 大きな画像の PNG のデコード時間の計測（T-054）。必要ならロックの外でデコードする方式（libghostty-vt の API の範囲では難しい）
