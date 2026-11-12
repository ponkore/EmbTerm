# T-032 / T-033 / T-041 / T-044 機能の残り（リサイズ・フォント・後始末・Emacs 側の操作）

- 日付: 2026-11-12
- **TODO §4「機能」の残り 4 つの記録**
- 結論:
  - **T-032 リサイズと reflow / T-041 後始末 / T-044 Emacs 側の操作**は、これまでの実装（T-028 / T-063 / T-073 / T-076 など）で足りていた。
    確かめていなかった場面を自動確認にして、どれも通った。コードの直しは無い
  - **T-033 フォント**は足した: フォールバックのフォントの指定（`embterm-font-fallback-families`）、`embterm-font-family` に候補のリスト、
    `text-scale-adjust` でバッファごとの拡大・縮小

## 1. T-032 リサイズと reflow（`test/resize.el`、NG 0）

pwsh 7（PSReadLine）+ 再配布版 ConPTY 1.25 で:

| 場面 | 結果 |
|---|---|
| 180 文字の行を 129 桁 → 62 桁（左右に分割）→ 129 桁 | 折り返し直される（2 行 → 3 行 → 2 行）。中身が欠けない・同じ出力が二重に出ない |
| 狭めた・広げた後の入力 | 出力がコマンドの次の行に出る（ConPTY との食い違いが無い） |
| 30 ms ごとに 20 回フレームの大きさを変える（ドラッグの真似） | 最後の大きさ（72x20）に落ち着き、pwsh の `$Host.UI.RawUI.WindowSize` も同じ |
| 40 行出してから縦に縮める → 戻す | 縮めると最後の行が見えたまま上へ押し出される。戻すと**空きが下に残る**（スクロールバックの行を画面に戻さない。`resize_pull_scrollback = false`。ConPTY の画面と食い違わないため。ghostel・Windows Terminal と同じ） |

間引き（デバウンス）は入れていない。大きさは描く前に毎回窓から決めるので、続けざまに変えても最後の大きさになる。

## 2. T-033 フォント（`test/font.el`、NG 0）

### 2.1 足したもの

| 設定 | 中身 |
|---|---|
| `embterm-font-family` | ファミリ名か、**ファミリ名のリスト**（前から順に、見つかったもの）。その後ろに既定の候補（HackGen Console NF → Cascadia Mono → Consolas）が続く |
| `embterm-font-fallback-families`（新設） | 主フォントに無い文字に、Windows のフォールバックより先に使うファミリ。無いものは飛ばす |
| `text-scale-adjust`（`C-x C-+` / `C-x C--` / `C-x C-0`） | そのバッファの端末だけ `embterm-font-size`（nil なら 12）× `text-scale-mode-step` ^ 倍率にする。customize でフォントを替えても倍率は残る |

- 描画（`crates/render/src/font.rs`）: `IDWriteFontFallbackBuilder` で「全 Unicode → 指定のファミリ」の対応を 1 つ作り、その後ろに
  システムのフォールバック（`GetSystemFontFallback`）を足して、4 つのスタイルの `IDWriteTextFormat1::SetFontFallback` に入れる。
  主フォントが持つ文字は今までどおり速い道（`DrawGlyphRun`）。フォールバックが効くのは主フォントに無い文字（遅い道の `IDWriteTextLayout`）だけ
- module の `create` / `set-font` は、ファミリとフォールバックを**文字列のベクタ**で受ける（引数が 1 つ増えた。embterm.el と同時に替わる）
- embterm.exe に `--font-fallback NAME`（繰り返し可）
- **合字は作らない**: セルごとに描くので、隣のセルの文字とはつながらない（要件の「合字はしない or 選べる」は「しない」に決めた）

### 2.2 確かめたこと

- Cascadia Mono（日本語も Nerd Font のアイコンも持たない）で `日本語   😀 ①★`（`target/t033/fallback-*.png`）:
  指定なしでは**アイコンが豆腐**、`Symbols Nerd Font Mono` を指定すると**アイコンが描かれる**。指定に無い絵文字は
  **これまでどおり Windows のフォールバックで色付き**で出る（指定の対応の後ろにシステムの対応が効いている）
- `'("No Such Font" "Cascadia Mono")` → Cascadia Mono を使う
- `text-scale-set 2` で 86x11 → 59x7、`0` で 86x11 に戻る。下に並べた別の端末は変わらない
- `customize-set-variable` で開いている 2 つの端末とも替わる

既定（HackGen Console NF）は日本語と Nerd Font のアイコンを両方持つので、ふだんはフォールバックの指定は要らない。

## 3. T-041 後始末（`test/cleanup.el` + `test/cleanup.sh`、NG 0）

kill-buffer と kill-emacs は `test/close.el`（T-073 / T-076）で確かめ済み。残っていた場面:

| 場面 | 結果 |
|---|---|
| シェルが自分で終わる（`exit`） | バッファが消え、子と**その端末の OpenConsole.exe も終わる** |
| `embterm-kill-buffer-on-exit` が nil | バッファは `[終了]` で残り、最後の画面が読める。あとで `kill-buffer` できる。OpenConsole.exe は残らない |
| **Emacs の異常終了**（`taskkill /F`。`kill-emacs-hook` が走らない） | cmd・pwsh（Store）・それぞれの子の ping・OpenConsole.exe 2 つが**すべて終わる**（Job のハンドルが閉じて `KILL_ON_JOB_CLOSE` が効く。OpenConsole.exe はパイプが切れて終わる） |

残りうるもの（T-073 の記録のとおり）: Job の外に出た子孫（cmd の中で `pwsh` と打って起こしたものの子など）は、
異常終了では親子関係で辿る手順が走らないので残りうる。

## 4. T-044 Emacs 側の操作（`test/frames.el`、NG 0。画面は `target/t044/frame-*.png`）

| 操作 | 結果 |
|---|---|
| `tab-bar-mode` でタブのバーが出る | 端末が本文の位置（18 px 下）へずれる |
| 別のタブへ移る | ペインは消え、端末は見えない。その間の出力は戻ったときに出ている |
| 子フレーム（corfu のポップアップの形: `parent-frame`・`no-accept-focus`・`undecorated`）を端末の上に出す | 子フレームが上に出る。下で端末が描き直しても隠れない（子 HWND は `HWND_BOTTOM`、以後は上げない） |
| 最小化（`iconify-frame`）→ 戻す | 最小化している間の出力が、戻したときに出ている。位置も同じ |
| 端末を出していたフレームを消す → 元のフレームに出し直す | 端末とプロセスは残り、出し直すと続きが出る（T-028 と同じ） |

残り: タブを移ると Lisp のウィンドウが変わるのでペインが作り直され、**そのウィンドウのスクロール位置は失われる**（下へ戻る）。T-087 に入れた。

## 5. 試験の道具

| ファイル | 中身 |
|---|---|
| `test/resize.el` | T-032 |
| `test/font.el` | T-033 |
| `test/cleanup.el` / `test/cleanup.sh` | T-041。sh が Emacs を起こし、ready を待って taskkill /F し、残りを tasklist で見る |
| `test/frames.el` | T-044 |
| `test/window-info.ps1` | 子 HWND が見えているかと、親のクライアント座標での位置 |

どれも `cargo xtask dist` の後に `emacs -Q -l test/<名前>.el`（cleanup は `bash test/cleanup.sh`）。結果は `target/t0xx/`。
