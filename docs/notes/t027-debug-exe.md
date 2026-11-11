# T-027 デバッグ用 exe（embterm.exe）と T-072 再配布版 ConPTY の取得

- 日付: 2026-11-11
- **TODO T-027 / T-072 の記録**（T-013・T-065・T-070 の一部も含む）
- 結論: **トップレベルのウィンドウ 1 枚 + 再配布版 ConPTY 1.25 + libghostty-vt + Direct2D で、cmd・pwsh・vim が動いた。**
  部品は `et-vt` / `et-pty` / `et-render` の 3 クレートに分け、embterm.exe はそれをつなぐだけにした

---

## 1. 作ったもの

| 場所 | クレート | 中身 |
|---|---|---|
| `crates/vt` | `et-vt` | libghostty-vt の FFI（**使う範囲だけ手書き**、T-065）と安全なラッパ。`Terminal`（生成・`vt_write`・resize・スクロール・モード・選択範囲・コールバック）、`RenderState`（更新・dirty・色・カーソル・行とセルの反復・書記素・スタイル・行の選択範囲）、`KeyEncoder`（特殊キー）。build.rs は `vendor/ghostty-vt` の静的ライブラリをリンク（`GHOSTTY_VT_PREFIX` で差し替え可） |
| `crates/pty` | `et-pty` | ConPTY。`conpty.dll` を指定のディレクトリの順に `LoadLibraryExW`、無ければ kernel32。overlapped の名前付きパイプ、`STARTF_USESTDHANDLES`、`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`、**Job Object（`KILL_ON_JOB_CLOSE`）**、終了の検知、`ResizePseudoConsole`、閉じるときは `ClosePseudoConsole` を別スレッドで呼んで EOF まで読む（時限付き）。読みはコールバック、書きは専用スレッド + チャネル |
| `crates/render` | `et-render` | HWND に DXGI のフリップモデル + Direct2D / DirectWrite で描く。`Frame`（render state から変わった行だけ写す）と `Renderer`。GenPreview 2432604 の `gpu.rs` / `surface.rs` をコピーして作り替えた（T-015。ファイルの先頭に出典） |
| `apps/embterm` | `embterm`（`embterm.exe`） | デバッグ用 exe。`session.rs`（VT と PTY のつなぎ。HWND に依存しない）、`window.rs`（ウィンドウ・入力・タイマー）、`keys.rs`、`args.rs` |
| `xtask` | — | `cargo xtask fetch-conpty [--force]` を足した（T-072） |

workspace の `members` に 4 つ足した。外部クレートは `windows`（workspace の 0.62）と、Direct2D の `Vector2` / `Matrix3x2` のための `windows-numerics` 0.3 だけ。

### 1.1 et-vt（T-065）

- 宣言は `crates/vt/src/ffi.rs`。enum は `c_int`、ハンドルは不透明なポインタ、共用体は Rust の `union`（`GhosttyStyleColor` の値だけ `[u8; 3]`）
- **`#[test]` で手書きの構造体を `ghostty_type_json()` と照合する**（`crates/vt/src/tests.rs`）。大きさ・揃え・各フィールドの位置・フィールドの数、
  共用体の大きさと揃え、使う列挙の値（`GhosttyTerminalOption`・`GhosttyRenderStateData`・`GhosttyKey` など）。JSON はテストの中の小さな読み取りで読む（外部クレートを足さない）
- 動きのテスト: eaw（`①★※─`）が 1 桁、DA1 の応答（`CSI ?62;22c`）、**`CSI 16 t` の応答（`CSI 6;18;9t`。SIZE コールバックで `resize` に渡したセルの画素を返す。T-070）**、
  OSC 2 と BEL、render state のセル（太字・パレット色・幅 2・spacer tail・反転・行の選択範囲）、スクロールバック、キーエンコーダ（`\x1b[A`、DECCKM で `\x1bOA`、`Ctrl+→` で `\x1b[1;5C`、Backspace は 0x7f）
- コールバックは userdata（`Box::into_raw` した `Inner`）経由で `Effects` トレイトに渡す。C の境界は `catch_unwind` で包む
- 途中でつまずいたこと: `RenderState::new` で `Self { rows, ..s }`（Drop を持つ型の構造体更新）を書いたら、古い `s` が Drop されて二重解放になった（`STATUS_HEAP_CORRUPTION`）

### 1.2 et-pty

- 手順は ghostel の `ConPtyProcess.zig` と同じ。足したもの: **Job Object**（子を `CREATE_SUSPENDED` で作ってから Job に入れて動かす。孫を取りこぼさないため）と、**時限付きの後始末**
  （EOF まで最大 3 秒 → `ClosePseudoConsole` のスレッドを最大 2 秒待つ → 子を 2 秒待ち、残っていれば `TerminateJobObject`）
- `Pty::close(timeout)` は止めの知らせ（イベント）を立て、読みのスレッドの終わりを `timeout` まで待つ。間に合わなければ Job ごと止めてスレッドは置いていく。`Drop` でも 3 秒で呼ぶ
- 環境変数は親の環境に `TERM=xterm-256color`、`COLORTERM=truecolor`、`TERM_PROGRAM=EmbTerm` を足す（名前の大文字小文字を無視して上書き、並べ替え）。コマンドラインは MSVCRT の規則でクォートする
- `ConptyKind` で「再配布版（OpenConsole.exe の有無）か OS 同梱か」を返す
- テスト: クォート、環境ブロック、**OS 同梱の ConPTY で `cmd /c echo` が出力と終了コード 0 を返す**、**再配布版で起動直後に `CSI c` が届き、`close` で子が止まる**

### 1.3 et-render（T-008・T-013）

- 座標はすべて物理ピクセル（デバイスコンテキストの DPI を 96 に固定し、フォントの大きさのほうを DPI で拡大する）。セルは整数の画素に置く
- フォントは候補の順（`--font` → "HackGen Console NF" → "Cascadia Mono" → "Consolas"）に最初に見つかったもの。セルの幅は `M` の送り幅、高さは ascent + descent + lineGap（丸め）。
  ユーザーのフォント（`%LOCALAPPDATA%\Microsoft\Windows\Fonts` の HackGen）も `GetSystemFontCollection` で見つかった
- 背景: 同じ色が続くセルをまとめて `FillRectangle`
- 文字の速い道: 主フォント（太字・斜体のフェイスも）が持つ 1 コードポイントの文字は、グリフ番号を引き、同じ色・同じフェイス・同じ大きさが続く範囲を 1 回の `DrawGlyphRun` にする。
  送り幅は「次のグリフの x − この x」にするので、フォントの送り幅には頼らずセルの位置に置かれる
- 文字の遅い道: 主フォントに無い文字・書記素クラスタ（結合文字、ZWJ、VS16）は `IDWriteTextLayout`（ロケール ja-jp、`D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT`）。
  **フォールバックは DirectWrite のシステムのフォールバックに任せた**。レイアウトは（文字列, スタイル）で覚える
- **幅 2 のセルは 2 セル分の枠**、幅 1 は 1 セルの枠に置き、枠の中で左右の真ん中に寄せる
- **T-013 の縮め方**: 枠より送り幅が広い（0.5px を超える）なら、`min(枠 / 送り幅, セルの ascent / インクの ascent, セルの descent / インクの descent)` で縮める（1 を超えない）。
  ghostel はフォントの ascent / descent と比べるが、ここでは**グリフのインクの範囲**（速い道はデザイン単位のサイドベアリング、遅い道は `GetOverhangMetrics`）と比べる
  - **主フォントのグリフで幅が枠に収まるものは縮めない**（罫線 `┌─┐` や Powerline の記号は上下いっぱいに描かれていて、縮めると隣とつながらない。最初はつながらなかった → 直した）
  - **右隣を借りる規則（ghostel と同じ）も入れた**: 幅 1 の文字のグリフが広く、左が空白（または行頭）で右が空白のときは 2 セル分の枠で描く（`Theme::borrow_right`）
- 太字・斜体（主フォントの Bold / Italic のフェイス）、下線（一重・二重・波線。点線と破線は一重で代用）と下線の色、取り消し線、上線、反転、不可視、薄い（背景と半々に混ぜる）、256 色・true color
- カーソル: フォーカスがあればブロックはセルの色を反転、無ければ枠だけ。下線・縦棒。幅 2 の文字の上なら 2 セル分。**点滅はしない**（T-035）
- 選択範囲: render state の行ごとの範囲の背景を `#264f78` にする
- 描くのは render state が dirty のとき（`Frame::update` が変わったと答えたとき）だけ。描いたら `render_state_clean`。ただし**描くときは毎回画面全体を描く**（部分の描き直しはしない）
- 画面の保存（デバッグ用）: `EndDraw` の後・`Present` の前に後ろのバッファを CPU で読めるビットマップへ `CopyFromBitmap` し、**圧縮しない PNG** を自前で書く（WIC も外部クレートも使わない）

---

## 2. 構成（スレッド）

T-006 の初版のとおり。

```text
PTY 読みスレッド（et-pty）            UI スレッド（HWND）                          書きスレッド（et-pty）
────────────────────────            ─────────────────────                       ────────────────────
ReadFile(64 KiB) ─▶ on_output         WM_APP_OUTPUT                                チャネルから受けて WriteFile
  端末をロック → vt_write               take_output（AtomicBool を落とす）            ▲
  （WRITE_PTY は書きのチャネルへ積む）   端末をロック → render_state_update → 外す     │ キー・フォーカス・端末の応答
  AtomicBool が false なら              Frame::update（dirty な行だけ写す）          │
  PostMessage(WM_APP_OUTPUT) 1 回       Renderer::draw → Present(1)                 │
子の終了 → ClosePseudoConsole（別スレッド）→ EOF → on_exit → PostMessage(WM_APP_EXITED)
```

- 端末の応答（DA1・CPR・`CSI 16 t`）は `WRITE_PTY` から書きのスレッドのチャネルに積むだけで、ロックを持ったまま PTY に書かない
- 読みのスレッドは、書く口ができるまで最初の 1 回だけ待つ（`OnceLock::wait`）。起動直後の `CSI c` への応答を落とさないため
- 知らせはすべて HWND 宛ての `PostMessage`（`PostThreadMessage` は使わない）
- ウィンドウプロシージャは `catch_unwind` で包む。`App` は `thread_local!` の `RefCell` で、再入（`SetWindowPos` → `WM_SIZE`）は `DefWindowProc` に任せ、
  描く直前にクライアント領域とスワップチェーン・桁数を見比べて取りこぼしを拾う（GenPreview §1.3 #1〜#3 と同じ）
- **`WM_DESTROY` は `App` を介さずに処理する**。`--exit-after` のタイマーの中（`App` を借りている間）で `DestroyWindow` すると `WM_DESTROY` が同期で届き、
  借りられずに `DefWindowProc` に回って `PostQuitMessage` が呼ばれず、exe が終わらなかった（最初の実行で踏んだ）

## 3. embterm.exe の使い方

```text
embterm.exe [--conpty-dir DIR | --system-conpty] [--font NAME] [--font-size PT] [--cols N --rows N] [--cwd DIR]
            [--input STR]... [--input-delay MS] [--input-interval MS]
            [--select C1,R1,C2,R2] [--resize C,R] [--resize-after MS] [--scroll-up N]
            [--screenshot PNG] [--screenshot-after MS] [--exit-after MS] [--log FILE] [-- コマンド 引数...]
```

- 既定のコマンドは `cmd.exe`、大きさは 100x30、フォントは 12pt
- `conpty.dll` は `--conpty-dir` → exe の隣 → （開発中は）リポジトリの `vendor/conpty` の順に探す。`--system-conpty` で OS 同梱を使う（見比べ用）
- キー: `WM_CHAR` → UTF-8。矢印・Home/End・PgUp/PgDn・Insert/Delete・F1〜F12・Enter・Backspace・Tab・Esc は libghostty-vt のキーエンコーダ（端末のモードに合わせる）。
  Ctrl+文字は Windows が作る制御文字、Alt+文字は ESC を前に付ける。Shift+PgUp/PgDn とホイール（3 行）でスクロールバックを遡る。キーを打つといちばん下へ戻る
- フォーカスの出入りで `CSI I` / `CSI O` を必ず送る（T-070）
- 左ドラッグで選択範囲（表示だけ。コピーは T-036）
- タイトルは OSC 0/2 の値。子が終わると `[終了: コード N]` を付ける（ウィンドウは閉じない）
- 自動確認用: `--input` はエスケープ（`\r` `\n` `\t` `\e` `\\` `\xHH` `\u{…}`、知らないものはそのまま）を解釈して PTY に直接送る。
  `--screenshot` は PNG と、画面のテキスト（同じ名前の `.txt`）を書く。`--select` / `--scroll-up` は保存の直前、`--resize` は `--resize-after` ms 後
- 起動から 3 秒後に「起動直後に `CSI c` が来たか」をログに出す（T-072。来れば再配布版、来なければ OS 同梱の見込み）

## 4. T-072: 再配布版 ConPTY の取得

- `cargo xtask fetch-conpty [--force]`。NuGet `Microsoft.Windows.Console.ConPTY` **1.25.260930003** を nuget.org の flat container から取る
- **SHA256 は `02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee`**（2026-11-11 に取ったもの。`xtask/src/main.rs` に書いた）
- パッケージの中（実際に開いて確かめた）: `runtimes/win-x64/native/conpty.dll`、`build/native/runtimes/x64/OpenConsole.exe`（ほかに arm64 / x86、`inc/conpty.h`、`.signature.p7s`）
- どちらも `Get-AuthenticodeSignature` で **Valid / `CN=Microsoft Corporation`**、ファイルの版は 1.25.2609.30003
- 置き場所: `vendor/conpty/conpty.dll` + `vendor/conpty/x64/OpenConsole.exe`（+ `VERSION.txt`）。conpty.dll の読み込みの規則（docs/research/conpty.md §1.3）に合う
- 展開は Windows の `tar.exe`（bsdtar）をフルパスで呼ぶ。Git for Windows の GNU tar は zip（.nupkg）を開けない

## 5. 確かめたこと

`cargo build`（workspace 全体、debug / release）、`cargo clippy`（新しいクレート、警告なし）、`cargo test -p et-vt -p et-pty -p et-render -p embterm`（6 + 4 + 2 + 2 件、すべて通る）。

embterm.exe を自動確認の引数で起動し、保存した PNG を目で見て確かめた。**PNG は `C:\Projects\EmbTerm\target\t027\`**（`target/` は git の対象外。下のコマンドで作り直せる）。
特に書かない限り再配布版 ConPTY 1.25、HackGen Console NF 12pt、DPI 96。

| PNG | 何をしたか | 見えたもの |
|---|---|---|
| `cmd1.png` | cmd で `echo ①あ★─`、`dir` | プロンプト、`①あ★─` のエコーと出力、日本語の `dir` の出力、ブロックのカーソル。桁ずれなし |
| `cmd-cascadia.png` | `--font "Cascadia Mono"`、`echo ①あ★─※ x①x ★ end` | `①★※` はフォールバックのグリフを **1 セルに縮めて**描く。孤立した ` ★ ` は**右隣の空白を借りて 2 セルで**描く |
| `cmd-consolas.png` | `--font Consolas --font-size 14`、同じ文字列、`--select 0,1,9,2` | 同上。選択範囲（2 行目全体と 3 行目の 10 桁）の背景 |
| `pwsh.png` | `-- pwsh -NoLogo`、SGR を並べた `Write-Output`、`Get-ChildItem` | 太字の赤・斜体・下線・取り消し線・反転・true color・256 色の背景・赤い波線の下線・薄い・上線、PSReadLine の色、ディレクトリ名の背景色 |
| `vim.png` | `-- vim -u NONE -N -n --cmd "set enc=utf-8" <一時ファイル>`、`:set nu cursorline`、挿入、`:syntax on` | 代替画面の全画面表示、行番号、cursorline の下線、構文の色、行末に入らない幅 2 の文字の `>`、カーソル |
| `resize.png` | cmd 100x20 で `dir /w` と長い `echo` → `--resize 60,14` | 60 桁で折り返し直された（reflow）画面。ConPTY 側も 60 桁で描き直す |
| `scroll.png` | cmd で 40 行出力 → `--scroll-up 15` | スクロールバックを 15 行遡った画面（`line 17`〜`line 28`） |
| `keys.png` | 別のプロセスから `PostMessage` で `WM_CHAR`（`echo keyed あ①`）と `WM_KEYDOWN`（Enter・↑・Home・Delete）を送った | **キー入力の経路**: 文字、Enter（後に来る `WM_CHAR` は捨てる）、↑で履歴、Home・Delete・`E` で `Echo keyed あ①` に直して実行 |
| `inbox.png` | `--system-conpty` | OS 同梱でも cmd は動く。ログは「`CSI c` が来ない（OS 同梱の見込み）」。再配布版では「受けた」 |
| `emoji.png` | pwsh で絵文字・ZWJ・結合文字・VS16・Nerd Font・罫線・ブロック要素 | 色付きの絵文字（セルの高さに縮む）、ZWJ の家族が 1 つ、`é`、`❤️`、Powerline の記号、罫線が隣とつながる表 |
| `release.png` | release ビルド（LTO、2.9 MB）で cmd | debug と同じ |

ほかに確かめたこと:

- 閉じるとき（`--exit-after`）: ウィンドウを壊してから exe が終わるまで約 20〜30 ms。子の cmd と OpenConsole.exe は残らない
- **Job**: cmd の `start /b ping …` で起こした孫は、閉じたときに止まった。**pwsh の `Start-Process … ping` で起こしたものは残った**（§6）
- 再配布版のログ: `ConPTY: 再配布版 C:\Projects\EmbTerm\vendor\conpty\conpty.dll` と「起動直後に CSI c を受けた」
- et-pty のテストで、再配布版の `close` の後に子のプロセスが無いこと

作り直し方の例（リポジトリの直下で）:

```sh
cargo xtask fetch-ghostty-vt && cargo xtask fetch-conpty && cargo build -p embterm
./target/debug/embterm.exe --log target/t027/cmd1.log --input 'echo ①あ★─\r' --input 'dir\r' \
  --screenshot target/t027/cmd1.png --exit-after 6000
./target/debug/embterm.exe --cols 80 --rows 16 --input-delay 2500 --input ':set nu cursorline\r' \
  --input 'ihello ①あ★─ world\e' --input ':syntax on\r' --screenshot target/t027/vim.png \
  --screenshot-after 5500 --exit-after 6500 -- vim -u NONE -N -n --cmd "set enc=utf-8" <一時ファイル>
```

キー入力の確認（`keys.png`）は、pwsh で `FindWindowW("EmbTermWindow")` → `PostMessageW` を呼ぶ小さなスクリプトを embterm.exe と並べて走らせた
（`TranslateMessage` は投げた `WM_KEYDOWN` からも `WM_CHAR` を作るので、本物のキー入力に近い）。スクリプトはリポジトリに入れていない。

## 6. 未解決の問題・気づいたこと

1. **pwsh の `Start-Process` で起こしたプロセスが Job から抜けた。** この環境の embterm.exe は外側の Job（`JOB_OBJECT_LIMIT_BREAKAWAY_OK` = 0x800）の中で動いている。
   入れ子の Job で抜けられた理由は未確認（ShellExecute 経由で `CREATE_BREAKAWAY_FROM_JOB` が付く？）。Emacs の中でも同じことが起きうる → T-041 で調べる
2. **閉じると子は強制終了に近い。** vim を `--exit-after` で閉じると `.swp` が残った（`ClosePseudoConsole` → 3 秒で EOF にならなければ Job ごと止める）。
   バッファを消すときに確かめる・猶予を置くなどは T-041。また `Pty::close` は UI スレッドで最大で数秒止まる（デバッグ用 exe では許したが、Emacs のスレッドでは不可）
3. **DEC 2026（同期出力）の render hold を使っていない。** `RENDER_HOLD` のコールバックを登録しておらず、描く直前の端末の状態をそのまま写すので、全画面のアプリで描きかけのフレームが見えうる
4. **描くたびに画面全体を描く**（行ごとの dirty は「写す」ところだけで使う）。`Present(1)` は垂直同期を待つので、出力が続くと UI スレッドが 1 フレームずつ止まる。負荷は T-068 で測る
5. **キーは最小**: win32-input-mode（T-069）が無いので、Ctrl+Space・Shift+Enter などコンソールのアプリが区別したい修飾キーは届かない。IME の変換中の表示も無い（`WM_IME_CHAR` → `WM_CHAR` の確定文字列だけ）
6. **マウスの報告をしない**（T-038）。ホイールはいつもスクロールバックを動かす（vim の代替画面では何も起きない）。選択は表示だけでコピーしない（T-036）
7. カーソルの点滅、ベル（T-040）、OSC 7 の扱い（ログに出すだけ）、DA1 / XTVERSION の名乗り（libghostty-vt の既定 `CSI ?62;22c` のまま。T-071）は手を付けていない
8. **DPI の変化（`WM_DPICHANGED`）はコードを書いたが、150% のモニタでは確かめていない**（T-043）
9. 色付きの絵文字（Segoe UI Emoji）はインクがセルより高いので、縮められて少し小さく見える。点線・破線の下線は一重で代用
10. デバイスを失ったときの作り直しは書いたが確かめていない。GPU のデバイスはウィンドウごと（T-067）
11. `conpty.dll` の探し先にリポジトリの `vendor/conpty`（ビルド時のパス）を入れている。配布物（T-030 の手順）では外す
12. HackGen Console NF では `①★※` が半角のグリフなので縮める処理は働かない（小さく描かれるのはフォントの形）。縮める・借りるの規則は Cascadia Mono / Consolas（フォールバックのグリフ）で確かめた

## 7. TODO に足すべき新しい課題

- **Job から抜けるプロセス**: 外側の Job が breakaway を許すとき、pwsh の `Start-Process` で起こした孫が Job から抜けた。原因を調べ、抜けさせない方法（`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` を外側で確かめる、Job を入れ子にしない起動など）を決める（T-041 の一部でもよい）
- **render hold（DEC 2026）**: `GHOSTTY_TERMINAL_OPT_RENDER_HOLD` を登録し、hold の始まりで render state を写して、終わるまで（または 1 秒の時限まで）写し直さない（libghostty-vt の guide-rendering の手順）
- **et-session（仮）**: `apps/embterm/src/session.rs`（Terminal + Pty + 読み書きのスレッド + 知らせ）を Emacs module と共有するクレートに移す（T-028 の前）
- **閉じるのを UI スレッドから外す**: `Pty::close` の待ち（最大数秒）を別スレッドで行い、終わったら知らせる（module では Emacs を止めないため）
- **部分の描き直し**: dirty な行だけ描く（`Present1` の dirty rect、またはフリップの前のバッファを保つ）かを T-068 の測定で決める
- **自動確認の道具をリポジトリに入れる**: `PostMessage` でキーを送るスクリプトと、screenshot の `.txt` を期待値と比べる簡単な回帰テスト（`cargo xtask` から呼べる形）
- **画面の保存の PNG を圧縮する**か WIC にする（今は無圧縮で 1〜2 MB）。デバッグ用なので優先度は低い
