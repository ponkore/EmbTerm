# EmbTerm 要件

- 日付: 2026-11-11（初版）
- 出典: [project-memo.txt](project-memo.txt)
- 状態: T-002 で見直し済み（2026-11-11）。決まっていない点は [TODO.md](TODO.md) で詰め、決まったらここを書き換える

**凡例**

| 印 | 意味 |
|---|---|
| 【確定】 | メモに書いてあること |
| 【決定】 | TODO.md で決めたこと（番号を付ける） |
| 【案】 | 整理するときに補ったこと。T-002 で了承済み。根拠は GenPreview と `~/.emacs.d/docs/` の調査記録。調査・実装の結果で変わりうるので、TODO.md の番号で確かめる |

---

## 1. 概要

**EmbTerm（Embedded Term）は、Windows の Emacs の中で動く端末。**
Emacs の dynamic module として読み込み、Emacs のウィンドウの本文領域に module が作った
子ウィンドウ（HWND）を重ね、そこに端末の画面を直接描く。

```text
Emacs (Lisp)                       embterm module (Rust)                     子プロセス
─────────────                      ──────────────────────                    ──────────
embterm.el                          端末 1 つにつき:
  位置合わせ・キー・設定  ──────▶     子 HWND（描画）
                         ◀──────     VT の解釈（libghostty-vt）  ◀── ConPTY ──▶  pwsh / claude / vim …
  （状態はポーリングで読む）          PTY の読み書き
```

## 2. 背景と目的

- いまは ghostel（libghostty-vt を載せた Emacs module）を使っている。
  **EmbTerm がうまくいけば ghostel はやめる**【確定】
- ghostel は端末の画面を Emacs のバッファのテキストとして描くため、セルの幅・グリフの大きさは
  Emacs の表示の仕組みに縛られる（例: `★` `※` は 1 セルに収めるため高さ 47% に縮められる。
  `~/.emacs.d/docs/pty/ghostel.md`）
- GenPreview で「Emacs のフレームに子 HWND を重ね、module が描く」方式が実用になった
  （`~/Projects/GenPreview/docs/emacs-embedded-view.md`）。**同じ方式を端末に使う**【確定】
- とくに claude は `①` などの East Asian Ambiguous（以下 eaw）の文字を半角 1 桁として扱う。
  Emacs の `char-width-table` は全体で 1 つなので、「端末の中だけ幅 1、ほかは幅 2」が
  Emacs では作れない。**自前で描けば Emacs の文字幅に縛られない**【確定】

## 3. 利用環境・前提

| 項目 | 内容 |
|---|---|
| OS | Windows（11 で使う）【確定】。**macOS は移植中**（T-124 で VT と PTY、T-125 で描画と窓、T-126 で Emacs の中の端末まで通った。残りは T-127 = IME・マウス・画像など）。Linux は対象外【案】 |
| Emacs | GNU 公式の Windows ビルド、31.1 以降（`x86_64-w64-mingw32`）【案: GenPreview と同じ】。ソース `C:/Apps/emacs/src/emacs-31.1/` |
| フレーム | GUI フレームだけ。`emacs -nw` は対象外【案】 |
| アーキテクチャ | x86_64。arm64 は後回し【案】 |

## 4. 当面の目標（受け入れ基準）

**次のアプリが、Emacs の中で普通の端末（Windows Terminal / WezTerm）と同じように使えること**【確定】。

| アプリ | とくに確かめること【案】 |
|---|---|
| cmd | 起動・入力・日本語の表示、コードページ |
| PowerShell（5.1）/ pwsh | PSReadLine の行編集・補完・色、starship のプロンプト（Nerd Font） |
| nyagos | 行編集・補完が動くこと（`M-x shell` では使えなかった） |
| wsl bash | `wsl.exe` 経由の起動、Linux 側の TUI、`TERM` |
| git bash | MSYS2 の bash（`bash.exe --login -i`）、less / vim |
| claude | 桁ずれしないこと（`①` 等の eaw、罫線、スピナー）、日本語の入力（IME）、長い出力の速さ、通知 |
| yazi | 画面全体の TUI、マウス、**画像のプレビュー**（Kitty graphics など） |
| vim | 代替画面、カーソルの形、マウス、`Esc` の遅延が無いこと、256 色 / true color |

## 5. 機能要件

### 5.1 構成

- **Emacs から呼ぶ module が本体**【確定】（`embterm-module.dll` + `embterm.el` の名前は【案】）
- **単体で動くデバッグ用の .exe も作る**【確定】。描画・VT・PTY は module と同じコードを使い、
  トップレベルのウィンドウで動く。機能や設定項目は最小限でよい
- module と exe で共有できる部分はクレートに分ける【案】

### 5.2 埋め込み表示【確定: 方式 / 案: 細部】

GenPreview の設計と HWND 埋め込みのプローブ（`~/.emacs.d/docs/embed/hwnd-child-window.md`）の
約束に従う。

- `(frame-parameter nil 'window-id)` のフレームを親に `WS_CHILD` のウィンドウを作り、
  `window-inside-pixel-edges` に合わせて置く
  （**macOS は `EmacsView` のレイヤーに CAMetalLayer を足す**。決定 T-126。マウスの当たり判定に出ないので Emacs の操作をそのまま残せる）
- 位置の追従は `window-size-change-functions` と `window-buffer-change-functions` で行う
  （分割・リサイズ・タブ・バッファの切り替え）
- 子ウィンドウは `HWND_BOTTOM` に置き、上げない（corfu などの子フレームを隠さないため）
- バッファが見えなくなったら子ウィンドウを隠す
- クリックしたら、その子ウィンドウを持つ Emacs のウィンドウを選ぶ

**同じ端末を複数のウィンドウに出したとき**【決定 T-016】

- **split（`C-x 2` / `C-x 3`）には対応する。** 分けたそれぞれのウィンドウに端末が見え、
  **ウィンドウごとに別のスクロール位置**で見られること（片方は最新の出力、片方はスクロールバックを遡る、など）
- **複数のフレームに同じ端末を出したときは、きれいに見えなくてよい**（片方にしか出ない、
  プレースホルダになる、などで構わない）
- 実装は別途決める（T-063）。「1 つの HWND の中を複数のペインに分けて描く」が落としどころの候補

### 5.3 端末エミュレーション

- **VT の解釈は、使えるなら libghostty-vt を使う**【確定】（T-005）
- 代替画面、スクロール領域、カーソルの形、256 色 / true color、太字・斜体・下線（種類と色）、
  ブラケットペースト、フォーカスの報告、同期出力（DEC 2026）【案】
- OSC: タイトル（0/2）、カレントディレクトリ（7）、ハイパーリンク（8）、通知（9 / 777）、
  クリップボード（52）【案】
- Kitty keyboard protocol、Kitty graphics（yazi の画像）【案】（T-022, T-023）
- 画面サイズが変わったら、再折り返し（reflow）してプロセスに伝える【決定 T-032】。主画面は libghostty-vt が折り返し直し、ConPTY にも同じ大きさを伝える。
  縦に広げてもスクロールバックの行は画面に戻さない（ConPTY の画面と食い違うため。ghostel と同じ）

### 5.4 PTY・プロセス

- PTY は ConPTY【決定 T-007】。**再配布版の `conpty.dll` + `OpenConsole.exe`（1.25 以降）を同梱して優先**し、
  無ければ OS の ConPTY を使う。OS 同梱版は Kitty graphics・Sixel を捨て、OSC の位置をずらし、
  結合文字・ZWJ の幅も揃わないので、退避先として動けばよい
- 起動するコマンド・引数・作業ディレクトリ・環境変数を elisp から指定できる【決定 T-109 / T-110】（`embterm-run`。子の環境は `process-environment` から作るので `setenv` が届く）
- バッファを消したらプロセス（とその子）を終わらせる。プロセスが終わったら Emacs に知らせる【決定 T-041】。
  子孫は Job に入れ、閉じるときは CTRL_CLOSE_EVENT → 猶予 → Job ごと止める（T-073 / T-076）。Emacs が異常終了しても子孫と OpenConsole.exe は残らない

### 5.5 文字幅（eaw）

- **claude が `①` を半角 1 桁で扱うことに合わせられること**【確定】
- セルの幅（何桁として数えるか）は Emacs の `char-width-table` と無関係に、端末側で決める【案】
- **eaw は 1 桁で固定する**【決定 T-012】。libghostty-vt の幅の表は固定（eaw は幅 1）で、
  claude・再配布版の ConPTY も 1 桁で数えるため。2 桁にする設定は持たない
- 1 桁と数えた文字のグリフがフォントでは全角のとき（`①` `★` `※` など）の描き方を、設定で切り替えられる【決定 T-013】
  - **overlap（既定）**: 全角の縦横を保ったまま描き、桁送りは 1 セル（右隣へはみ出して重なる。WezTerm と同じ見え方）
  - **shrink**: 1 セルに収まるよう縮める（ghostel と同じ）
- ConPTY（conhost）・libghostty-vt・アプリの 3 者が同じ桁数で数えること【案】（T-011）

### 5.6 描画

- 子 HWND に直接描く。Direct2D / DirectWrite + DXGI のフリップモデル【決定 T-008】（**macOS は CAMetalLayer に Metal + CoreText のグリフアトラス**。決定 T-125）。文字は色ごとに画面全体でまとめ、背景はセル 1 つを 1 画素にしたビットマップで描く。GPU デバイスは端末どうしで 1 つを共有する（T-067）
- フォントのフォールバック（日本語・Nerd Font の記号）【決定 T-033】: 主フォントに無い文字は `embterm-font-fallback-families` のファミリ →
  Windows（DirectWrite）のフォールバックの順に探す。**合字はしない**（セルごとに描く）
- カーソル（ブロック・下線・縦棒、点滅）、選択範囲、IME の変換中の文字列の表示【案】
- 描くのは変わったときだけ。フレームレートは垂直同期まで（待てるスワップチェーンで UI スレッドを止めない）。描くときは毎回全体を描く【決定 T-068 / T-077】

### 5.7 入力

- キー: **フォーカスは Emacs に残し、Emacs のキーマップで受けて端末に送る**【決定 T-009】。
  端末に送るキーと、Emacs に残すキー（`C-x` など）を分けられる
- **日本語入力（IME）が使えること**。変換中の文字列と候補ウィンドウが端末のカーソルの位置に出ること【決定 T-010】（tr-ime advanced の位置の上書きを使う）
- マウス: クリック・ドラッグでの選択、ホイールでのスクロール、アプリへのマウスの報告（vim / yazi）【案】
- 貼り付け（ブラケットペースト）、ファイルのドロップは後回し【案】

### 5.8 選択・コピー・スクロールバック

- 範囲を選んでコピー（Emacs の kill-ring にも入る）【案】
- スクロールバックの行数を設定できる【案】
- 端末の内容を Emacs のバッファのテキストとして扱う手段（isearch、`M-w`、occur）を用意する。
  方式は TODO（T-017）【案】

### 5.9 elisp からの設定

- **フォント・色・テーマなどは elisp から設定する**【確定】
  - 例【案】: フォント（ファミリ・大きさ・フォールバック）、前景・背景・カーソル・選択の色、
    16 色のパレット、Emacs のテーマ（face）に追従するかどうか、カーソルの形、余白、
    スクロールバックの行数、ベル、広いグリフの描き方（overlap / shrink、§5.5）
- **Windows Terminal の設定画面・コマンドパレットや、WezTerm の lua のような仕組みは持たない。**
  拡張は elisp で行う【確定】。module は elisp から呼べる関数（と通知）を出すだけ
- タブ・ペインは Emacs のウィンドウ・タブで行う【決定 T-044】（tab-bar・子フレーム・最小化・フレームの削除で確かめた）

### 5.10 Emacs への通知

- タイトルの変化、カレントディレクトリ（OSC 7）、ベル、通知（OSC 9 / 777）、
  プロセスの終了、クリック、エラーを elisp で受け取れる【案】
- 受け取ったものの扱い（モードライン、`default-directory` の追従、通知の表示）は elisp で書く【案】

### 5.11 単体デバッグ用の .exe

- 引数でコマンドを指定して起動する。描画・入力・VT・PTY を Emacs 抜きで確かめるためのもの【確定】
- 設定はコマンドライン引数か最小限のファイル。GUI の設定画面は作らない【案】
- 可能ならログ・VT の記録（再生できる形）を取れる【案】

## 6. 非機能要件

### 6.1 スレッド【確定: emacs からの使用を想定したスレッドのハンドリング】

GenPreview とプローブで分かったことに従う【案】。

- **Emacs の Lisp スレッドを止めない。** Lisp → module は `PostMessage` で送るだけ。
  module → Lisp は共有の状態をタイマーでポーリングして読む
  （CRT を静的リンクした module では `open_channel` が使えない。GenPreview ADR 0006）
- **子 HWND を持つスレッドで重い処理をしない。** 入力キューが Emacs と連結されるので、
  詰まると Emacs のキー入力まで止まる
- module の別スレッドから Lisp を呼ばない
- スレッドの分け方（端末ごとに UI・PTY 読み・VT 解釈・描画をどう分けるか）は T-006

### 6.2 性能【案】（T-054 で満たすことを確認。docs/notes/t054-performance.md）

- 大量の出力（`cat` で数 MB）で Emacs が固まらない
- キー入力から表示までの遅れが Windows Terminal と同程度
- 端末を数個〜十数個開いても重くならない

### 6.3 安定性【案】

- module の panic で Emacs を落とさない（境界で `catch_unwind`）
- 読み込み済みの DLL は差し替えられないので、更新の手順を決めておく（T-030）

### 6.4 配布【案】

- module は DLL 1 枚（CRT は静的リンク）+ `embterm.el`。必要なら `conpty.dll` / `OpenConsole.exe`
- Emacs のビルドが MSVCRT でも UCRT でも動く

## 7. 実装方針

- **実装言語は Rust が望ましい**【確定】。制約があれば他も検討する【確定】
  （libghostty-vt は C ABI なので Rust から FFI で呼ぶ想定【案】）
- Emacs module は `emacs` クレート（GenPreview と同じ）【案】
- GenPreview のコード（埋め込み・Direct2D・module の作法）を流用できるところは流用する【案】

## 8. 対象外

- Windows Terminal / WezTerm のような設定画面、コマンドパレット、lua などの拡張言語【確定】
- 端末の中のタブ・ペイン（Emacs のウィンドウで行う）【案】
- `emacs -nw`【案】。Linux【案】（macOS は T-121 の検討のうえ移植中。TODO.md §9）
- ghostel の統合機能（`compile` / `shell-command` / comint を端末に置き換える）は当面持たない【案】

## 9. 参考

| | |
|---|---|
| `~/.emacs.d` | 今の設定。`user-lisp/my-ghostel.el`、`docs/pty/ghostel.md`、`docs/pty/my-pty.md` |
| `~/.emacs.d/docs/embed/hwnd-child-window.md` | 子 HWND を重ねるプローブの実測と「設計上の約束」 |
| `~/.emacs.d/docs/japanese/eaw-and-cp5022x.md` | eaw の扱い |
| `~/.emacs.d/docs/claude/` | claude を Emacs で使う検討（eaw の桁ずれの実測あり） |
| `~/Projects/GenPreview/` | 埋め込みの実装（`docs/emacs-embedded-view.md`、ADR 0006 / 0008） |
| `C:/Apps/emacs/src/emacs-31.1/` | Emacs のソース（`emacs-module.h`、w32 の IME・フレーム） |
