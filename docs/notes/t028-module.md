# T-028 Emacs module（embterm_module.dll）と T-029 embterm.el

- 日付: 2026-11-11
- **TODO T-075 / T-076 / T-028 / T-029 の記録**（T-030・T-031・T-066・T-070 の一部も含む）
- 結論: **デバッグ用の embterm.exe と同じ端末（ConPTY + libghostty-vt + Direct2D）を Emacs の dynamic module にし、
  Emacs のウィンドウの本文領域に子 HWND として重ねて動かせた。** cmd・pwsh が動き、分割・リサイズ・バッファの切り替え・
  フレームの削除・クリックでのウィンドウの選択・バッファの削除でのプロセスの終了を、別に起こした `emacs -Q` で自動で確かめた。
  大量の出力の最中も Emacs のタイマーの間隔は最大 20〜29 ms（何もしていないときは 17〜21 ms）で、固まらない

---

## 1. 作ったもの

| 場所 | クレート / ファイル | 中身 |
|---|---|---|
| `crates/session` | `et-session`（T-075） | `apps/embterm/src/session.rs` と `keys.rs` を移した。`Session`（端末 + PTY + 読み書きのスレッド + 知らせ）、`keys`（レガシーの VT のキーの変換） |
| `apps/embterm-module` | `embterm-module`（cdylib、`embterm_module.dll`） | Emacs module。`lib.rs`（`#[defun]` の橋渡し）、`term.rs`（端末ごとの UI スレッドと子 HWND） |
| `lisp/embterm.el` | — | `M-x embterm`、位置合わせ、キー、ポーリング、copy モード、設定（T-029） |
| `test/smoke.el` | — | 別の `emacs -Q` に読ませる自動確認（§5） |
| `test/capture-window.ps1` / `test/click-window.ps1` | — | Emacs のフレームを PrintWindow で PNG に保存する / 子 HWND に左クリックを `PostMessage` する（自動確認用。UTF-8 の BOM 付き。Windows PowerShell 5.1 は BOM が無いと cp932 で読んで壊れる） |
| `xtask` | — | `cargo xtask dist` を足した（release でビルドし `target/dist/` に embterm_module.dll・embterm.el・embterm.exe・conpty.dll・x64/OpenConsole.exe を集める） |
| `crates/render` | `et-render` | `Renderer::set_font` を足した（フォントの変更を開いている端末に反映するため） |

embterm.exe は `et-session` を使うように直した（`session.rs`・`keys.rs` を消した）。workspace の `members` に `apps/embterm-module` と `crates/session` を足した。

### 1.1 et-session（T-075・T-076）

- `Session` のメソッドはすべて `&self` にした（PTY は `Mutex<Option<Pty>>`）。**`Arc<Session>` にして UI スレッドと Lisp のスレッドで分け合える**。
  端末の状態は HWND と別の寿命になる（T-066）
- **閉じる処理を呼び出し側から外した（T-076）**: `Session::close()` は `session-close` スレッドで `Pty::close(3 秒)` を待ち、終わったら `Event::Closed` を知らせる。
  戻り値の `Closing::wait(timeout)` で待つこともできる（embterm.exe は終わる前に待つ）。`Drop` も待たない
- `Event` に `Pwd` と `Closed` を足した
- `keys`: 仮想キー → 特殊キー（embterm.exe 用）に加え、**Emacs のファンクションキーの名前 → 特殊キー**（`key_by_name`。`return` `prior` `deletechar` `backtab` `f1`〜`f12` など）、
  **1 文字 + 修飾キー → レガシーの列**（`encode_char`。Ctrl は `@`〜`_`・英字・空白・`/`・`?` を制御文字に、Meta は ESC を前に、Shift だけの英小文字は大文字に）、
  **貼り付け**（`paste_bytes`。改行を CR に、ブラケットペーストのモードなら `ESC [200~` 〜 `ESC [201~` で包み、中の終わりの印を取り除く）。単体テスト 3 件

### 1.2 embterm-module の関数（`embterm-module-*`）

| 関数 | 待つか | 中身 |
|---|---|---|
| `create ARGV CWD CONPTY-DIR FONT-FAMILY FONT-SIZE NARROW-WIDE` | 制御用のウィンドウができるまで（数 ms） | 端末の UI スレッドを立てる。**子プロセスは最初の `place` で起動する**（その大きさで起動し、起動直後のリサイズを避ける）。user-ptr を返す |
| `place TERM WINDOW-ID X Y W H` | 待たない | 親（フレーム）のクライアント座標に置いて見せる。親が変われば `SetParent` で付け替え、子 HWND が消えていれば作り直す |
| `hide` / `send-string` / `send-key KEY MODS` / `paste` / `scroll LINES` / `set-font FAMILY SIZE` / `set-narrow-wide MODE` / `focus FLAG` / `screenshot PATH` / `close` | 待たない | 頼みごとを列に積んで、制御用のウィンドウに `PostMessage` |
| `poll TERM GENERATION` | 待たない（写すだけ） | 世代が同じで知らせも無ければ nil。変わっていれば plist（`:generation :alive :view :hwnd :started :pid :conpty :font :title :pwd :exited :exit-code :closed :cols :rows :events`） |
| `cursor TERM` | 待たない | カーソルの矩形 `(X Y W H)`（子 HWND の中の画素）。IME の位置合わせ用（T-010。まだ使っていない） |
| `text TERM` | **端末をロックする** | 画面（ビューポート）のテキスト。デバッグ・自動確認・copy モード用 |
| `version` / `set-log FILE` | — | 版、ログのファイル（デバッグ用） |

`:events` は `(clicked)` `(bell)` `(exited . CODE)` `(screenshot . PATH)` `(error . MESSAGE)`。

## 2. 構成（スレッドと Lisp とのやりとり）

```text
Emacs の Lisp スレッド               端末の UI スレッド（1 端末に 1 本、embterm-term）            et-session のスレッド
──────────────────────             ───────────────────────────────────────────            ─────────────────────
embterm.el                          制御用のウィンドウ（HWND_MESSAGE。端末が生きている間ずっと）       pty-read: 読む → 端末をロック → vt_write
  フック・キー → module 関数 ──┐       WM_APP_REQUEST  列を全部取り出して片付ける                   └ PostMessage(制御, WM_APP_OUTPUT)（まとめて 1 回）
                              └─▶  Shared.requests + PostMessage(制御, WM_APP_REQUEST)          タイトル・cwd・子の終了 → PostMessage(制御, WM_APP_SESSION)
  タイマー 50 ms → poll ◀─────────  WM_APP_OUTPUT   render state を写して描く                    ベル・閉じ終わり → Shared に直接書く（Weak）
                                    WM_APP_SESSION  タイトルなどを Shared.state に書く           pty-write: チャネルから受けて書く
                                    表示用の子 HWND（WS_CHILD、親 = Emacs のフレーム）            session-close: Pty::close を待つ（T-076）
                                      WM_PAINT / WM_SIZE / クリック / ホイール / 選択
```

- **Lisp → module は待たない。** 頼みごとは `Mutex<Vec<Request>>` に積み、**HWND 宛ての `PostMessage`**（`PostThreadMessage` は使わない。モーダルループで取りこぼさないため）。
  投げたまま取り出されていない間は投げ直さない（AtomicBool）
- **宛先は表示用の子 HWND ではなく、メッセージ専用の制御用ウィンドウ。** 子 HWND は親のフレームを消すと一緒に壊れるが、制御用のウィンドウと
  UI スレッドと端末（`Session`）は残る。次の `place` で子 HWND を作り直す（T-066）。フレームを消して戻したとき、画面の中身もそのまま残っていた（§5）
- **module → Lisp は共有の状態をポーリング**（T-014 の初版）。世代は「タイトル・cwd・子の終了・大きさ・子 HWND の有無・知らせ」で増え、
  **出力やカーソルの移動では増えない**ので、何もしていない端末の `poll` は nil を返すだけ
- 子 HWND: `WS_CHILD | WS_CLIPSIBLINGS`、`WS_EX_NOACTIVATE | WS_EX_NOPARENTNOTIFY`、`WM_MOUSEACTIVATE` に `MA_NOACTIVATE`（`App` を見る前に返す）。
  作ったら `HWND_BOTTOM`、付け替えたらもう一度 `HWND_BOTTOM`、動かすときは `SWP_NOZORDER`。**DPI の設定（`SetProcessDpiAwareness` 系）は呼ばない**
- `App`（UI スレッドの状態）はスレッドローカルの `RefCell`。`SetWindowPos` などの再入は `DefWindowProc` に任せ、描く直前にクライアント領域と
  スワップチェーン・桁数を見比べる（embterm.exe と同じ）。子 HWND が借用中に壊れたときは控え（`DESTROYED_VIEW`）と `IsWindow` で気づく
- 大きさ: `place` の画素の大きさ → `Renderer::grid_size` → 変われば `Session::resize`（端末と ConPTY の両方）
- 見えていない（`hide`）間は描かない。見せたら全体を描く
- **panic（T-031）**: `#[defun]` はクレートが Lisp のエラーにする。UI スレッドの本体・2 つの wndproc・user-ptr の `Drop`（`Term::drop` は `Close` を積むだけ）・
  閉じるスレッドを `catch_unwind` で包む。スレッドが panic で終わっても `:alive` nil と `(error . …)` を必ず書く。`Drop` の中で `unwrap` しない
- ConPTY の場所は Lisp から渡す（`embterm-conpty-directory`、既定は `embterm-module-directory` = embterm.el の隣）。無ければ OS 同梱

## 3. embterm.el（T-029）

- `M-x embterm`: `embterm-shell`（既定は pwsh があれば `("pwsh" "-NoLogo")`、無ければ `("cmd.exe")`。文字列も可）を `default-directory` で起動する
  （リモートなら `~`）。`embterm-mode` のバッファは読み取り専用の置き物
- **module の読み込み（T-030）**: GenPreview と同じく、`embterm_module-<更新時刻>-<サイズ>.dll` を `temporary-file-directory` にコピーしてから `module-load`。
  古いコピーは消す（読み込み中のものは消せないので無視）。conpty.dll と OpenConsole.exe は元の場所のものを使う。新しい版を使うには Emacs の再起動が要る
- **位置合わせ**: `window-size-change-functions`・`window-buffer-change-functions`・`window-selection-change-functions`（グローバル）と 50 ms のポーリングで
  `embterm--place`。出すウィンドウは**選んでいるウィンドウ → 前に出していたウィンドウ → 最初のウィンドウ**（`get-buffer-window-list … 'visible`。別フレーム・タブも同じ規則）。
  `(window-id window edges)` が前と同じなら送らない。見えなければ `hide`
- **フォーカス（T-070）**: 「選んでいるウィンドウが端末のバッファ、かつフレームにフォーカスがある（`frame-focus-state` が nil でない）、かつ semi-char」のときだけ
  入ったとし、変わったときに `focus` を送る（module は `CSI I` / `CSI O` を必ず送り、カーソルを塗りつぶし / 枠にする）。`after-focus-change-function` とフックとポーリングで見る
- **キー（案 A）**: `embterm-semi-char-map` を `emulation-mode-map-alists` の先頭に置く（端末を開くたびとポーリングのたびに取り返す）。
  中身は ghostel の semi-char と同じ並び（ファンクションキーとその修飾、`RET` `TAB` `DEL` `<backtab>`、`C-<英字>`（`C-i` `C-m` `C-y` を除く）、`C-M-<英字>`、`M-<ASCII>` など）から
  `embterm-keymap-exceptions`（既定 `C-c` `C-x` `C-u` `C-h` `M-x` `M-:` `C-\`）を除いたもの。
  - 文字（IME で確定した文字を含む）は `self-insert-command` の remap で `send-string`
  - それ以外は `last-command-event` を `(KEY . MODS)` にして `send-key`。整数のイベントは Meta だけ外した文字（`C-a` は 1）を送り、文字にならない組（`C-SPC` `C-%`）は
    `event-basic-type` と修飾キーで送る。シンボルのイベントは `event-basic-type` の名前（`up` `return` …）と修飾キー
  - `C-y` / `S-<insert>` / `[remap yank]` は貼り付け、`[remap mwheel-scroll]` / `[remap pixel-scroll-precision]` は Emacs に来たホイールの転送
  - `embterm-mode-map`: `C-c C-c` / `C-c C-z` / `C-c C-d` / `C-c C-\` で制御文字を送る、`C-c C-k` で copy モード、`C-c C-j` で semi-char に戻る、`C-c C-y` で貼り付け
- **copy モード**: 子 HWND を隠し、`embterm-module-text` の画面のテキストをバッファに写して、キーを Emacs に返す（isearch や `M-w` が使える。スクロールバックは含まない。本格的には T-017）
- 知らせ: タイトル → バッファ名（`embterm-buffer-name-format`、既定 `*embterm: %s*`）、OSC 7 → `default-directory`（`file://HOST/C:/…` も解く）、
  クリック → そのウィンドウを選ぶ（別フレームなら `select-frame-set-input-focus`）、ベル → `ding`、プロセスの終了 → メッセージと、`embterm-kill-buffer-on-exit`（既定 t）ならバッファを消す
- バッファを消したら `close`（待たない）。`kill-emacs-hook` でも全部閉じる
- 設定: `embterm-font-family`・`embterm-font-size`・`embterm-narrow-wide`（customize で変えると開いている端末にも反映）、`embterm-shell`、`embterm-module-directory`、
  `embterm-conpty-directory`、`embterm-kill-buffer-on-exit`、`embterm-buffer-name-format`、`embterm-poll-interval`、`embterm-keymap-exceptions`
- 使った作法: byte-compile で警告なし（`emacs -Q --batch -f batch-byte-compile`）

## 4. 使い方（開発中）

```sh
cargo xtask fetch-ghostty-vt && cargo xtask fetch-conpty   # 初回だけ
cargo xtask dist                                           # target/dist/ に集める
```

```elisp
(load "c:/Projects/EmbTerm/target/dist/embterm.el")   ; module は embterm.el の隣を探す
(embterm)
```

## 5. 確かめたこと

`cargo build`・`cargo test --workspace`（et-session の 3 件を足して全部通る）・`cargo clippy --workspace --all-targets`（警告なし）。

**自動確認**: `C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/smoke.el`（ユーザーの Emacs とは別のプロセス。約 70 秒で自分で終わる。上限 150 秒）。
結果は **`C:\Projects\EmbTerm\target\t028\`**（`smoke.log`・`module.log`・PNG・テキスト）。キーは `unread-command-events` に積むので、**本物のキー入力と同じく
キーマップ（semi-char のキーマップ・`embterm-mode-map`）を通る**（OS のキー入力・IME は通らない）。フレームの PNG は PrintWindow（`PW_RENDERFULLCONTENT`）で取った。
HackGen Console NF 12pt、セル 8x18 px、DPI 96、再配布版 ConPTY 1.25。

| PNG | 何をしたか | 見えたもの・結果 |
|---|---|---|
| `cmd1.png` / `frame-cmd1.png` | cmd で `echo ①あ★─` と `dir`（キーマップを通して） | 本文領域にぴったり重なった端末。日本語の `dir`、109x29。タイトルがバッファ名 `*embterm: C:\WINDOWS\SYSTEM32\cmd.exe*` に |
| `frame-split-v-top.png` | `split-window-below`（上を選んだまま） | 上のウィンドウに端末（109x13）、下は置き物のテキスト |
| `frame-split-v-bottom.png` | `other-window` で下を選ぶ | 端末が下へ移った（109x14） |
| `frame-split-h.png` / `split-h.png` | `delete-other-windows` → `split-window-right` | 左に端末（52x29。cmd の出力が 52 桁で折り返し直された） |
| `frame-resize.png` / `resize.png` | フレームを 80x28 文字に縮めて `dir /w` | 79x22 に追従。`dir /w` が 6 列で出る |
| `frame-hidden.png` | `*scratch*` に切り替える | 端末が隠れ、`*scratch*` が見える |
| `frame-back.png` | 端末のバッファに戻す | 元どおり出る |
| `frame-copy.png` | `C-c C-k`（copy モード） | 端末が隠れ、画面のテキストがバッファに入る。モードラインに `[copy]`。`C-c C-j` で戻った |
| `font16-shrink.png` | `customize-set-variable` で `embterm-font-size` 16、`embterm-narrow-wide` shrink | 79x22 に変わり、`①★※` が 1 セルに縮む。戻すと 109x29 |
| `pwsh.png` / `frame-pwsh.png` | 2 つ目の端末で pwsh を起動し `Write-Output "pwsh ok $($PSVersionTable.PSVersion) ①あ★─"` | PSReadLine の色付きの入力行、`pwsh ok 7.6.9 ①あ★─` |
| `pwsh-flood.png` | pwsh で 30,000 行を出力 | 6 秒以内に出終わり、最後の行まで描かれた |
| `frame-two.png` / `frame-click.png` | 左に cmd、右に pwsh を並べて左を選び、右の子 HWND に `WM_LBUTTONDOWN` を `PostMessage` | **右のウィンドウが選ばれた。** 左のカーソルは枠（フォーカス無し）、右は塗りつぶし（フォーカスあり） |
| `frame2.png` | 新しいフレームに cmd を出す | 子 HWND が新しいフレームへ付け替わり（`SetParent`）、89x20 に |
| `after-frame-delete.png` / `frame-after-delete.png` | そのフレームを `delete-frame` → 元のフレームで cmd に戻して `echo after-frame-delete` | 子 HWND は壊れ（`:view` nil）、**端末とプロセスは残り**、戻すと子 HWND を作り直して前の画面の続きが出る |

ほかに `smoke.log` で確かめたこと:

- **Emacs が固まらないこと**: 10 ms ごとのタイマーの間隔の最大は、何もしていないとき 17〜21 ms、pwsh が 30,000 行を出している最中 20〜29 ms（3 回の実行）
- **バッファを消すと待たない**: `kill-buffer` 2 つで 0.6〜1.1 ms。5 秒後に子プロセス（cmd・pwsh）の pid が無い（`process-attributes` が nil）
- **プロセスが終わるとバッファが消える**: `cmd.exe /c exit 3` → 「プロセスが終わりました（*embterm*）、終了コード 3」のメッセージとバッファの削除
- **Emacs を終えると子も消える**: 端末を開いたまま `kill-emacs` → 子の cmd の pid が無い（Job の `KILL_ON_JOB_CLOSE`）
- T-030: `embterm_module-20261111132441-3046400.dll` のように一時ディレクトリにコピーして読んだ（元の `target/dist` の DLL はロックされない。
  ただし「試験用の Emacs を起こしたまま dist を作り直す」ことは確かめていない）
- embterm.exe（et-session に移した後）も今までどおり動く（`--input` `--screenshot` `--exit-after`。閉じる処理を待ってから終わる）

## 6. 未解決の問題・気づいたこと

1. **overlap（既定）で `★` の直後の `─` が見えない**（`①あ★─` が `①あ★` に見える）。embterm.exe でも同じなので et-render（T-013）の問題。shrink では見える（`font16-shrink.png`）
2. **split はビューが 1 枚だけ**（GenPreview と同じ）。ほかのウィンドウには置き物のテキストが見える。ウィンドウごとのスクロール位置は T-063
3. **キーはレガシーの VT だけ**（T-069）。`C-SPC`・`S-RET`・`C-S-<英字>` などは区別して送れない。`M-<ASCII>` を端末に送るので、端末のバッファでは Emacs の `M-` のコマンド
   （`M-x` `M-:` を除く）は使えない（ghostel と同じ）。`C-g` も端末に送る
4. **IME は確かめていない**（T-010）。確定した文字は `self-insert-command` の remap で 1 文字ずつ届く見込み。カーソルの矩形は `embterm-module-cursor` で取れるが、変換ウィンドウの位置合わせはまだ
5. **ホイールを Emacs が受けたときの転送**（`MouseWheelRouting` 次第）と、子 HWND で受けたホイール・ドラッグでの選択（表示だけ。コピーは T-036）は自動確認していない
6. **子 HWND を作るたびに `Gpu::create`**（D3D11 + D2D のデバイスの作成。フレームを消して作り直すときも）。端末をたくさん開くなら共有する（T-067）
7. `Present(1)`（垂直同期待ち）を UI スレッドで行う。Emacs のタイマーには響かなかったが、クリックやキーの遅れは測っていない（T-068）
8. **子プロセスは最初の `place` で起動する。** バッファを一度も表示しないと起動しない（`M-x embterm` は必ず表示する）。表示の前に文字を送ると 80x24 で起動する
9. `embterm-module-text` は Lisp のスレッドで端末をロックする（copy モード・デバッグ用。大量の出力の最中は数 ms 待ちうる）
10. **閉じると子は強制終了に近い**（T-076 の後半: vim の `.swp` が残る件）。今回は「待たない」ところまで
11. クリックからウィンドウが選ばれるまで最大 50 ms（ポーリングの間隔）
12. タイトルが長い（cmd は `C:\WINDOWS\SYSTEM32\cmd.exe`）とバッファ名も長い。短くする規則は決めていない
13. DPI（150% のモニタ）は確かめていない（T-043）。Emacs は system-aware なので、子 HWND の `GetDpiForWindow` も system の DPI を返す見込み
14. 自動確認で詰まったこと: `write-region` で cp932 にできない文字があると Emacs が書く前にコーディングシステムを尋ね、その間止まった（ウィンドウが 2 つに分かれて端末が 13 行になったのはこの問い合わせ）。
    smoke.el は `coding-system-for-write` を utf-8 にした

## 7. TODO に足すべき新しい課題

- **overlap で広いグリフの右隣の文字が消える**: `★─` の `─` が見えない（et-render、T-013 の続き）。描く順か、はみ出しの塗り方を確かめる
- **IME の確定文字と変換ウィンドウの位置**: `embterm-module-cursor` の矩形を tr-ime の `tr-ime-modadv--set-composition-window` に渡す（T-010 の実装。docs/research/emacs-ime.md §4）
- **copy モードでスクロールバックも写す**（今は画面だけ）。T-017 の方式を決めるときに一緒に
- **バッファ名の付け方**: タイトルが長いときの短縮（cmd の `C:\WINDOWS\SYSTEM32\cmd.exe`）、タイトルが無いときにコマンド名を使う、など
- **自動確認を `cargo xtask` から呼ぶ**（T-078 の一部。`cargo xtask dist` → `emacs -Q -l test/smoke.el` → `smoke.log` の確認）。期待値との比較はまだ目視
- **子 HWND のホイール・選択・Emacs 側のホイールの転送の自動確認**（click-window.ps1 と同じく `PostMessage` で送れる）
