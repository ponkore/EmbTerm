# T-034 / T-035 / T-039 / T-040 / T-042 / T-080 / T-083 Emacs との統合の残り

- 日付: 2026-11-11
- **TODO T-034（色とテーマ）、T-035（カーソル）、T-039（OSC）、T-040（ベル）、T-042（複数の端末）、T-080（バッファ名）、T-083（tr-ime の再変換）の記録**
- 結論: **libghostty-vt 1.0 の C ABI のコールバック（`CLIPBOARD_WRITE` / `CLIPBOARD_READ` / `DESKTOP_NOTIFICATION` / `PROGRESS_REPORT`、
  `COLOR_*` / `COLOR_PALETTE`、`DEFAULT_CURSOR_STYLE` / `_BLINK`、`ghostty_grid_ref_hyperlink_uri`）だけで足り、自前の OSC の走査は要らなかった。**
  Lisp → module はすべて頼みごとを積むだけ（待たない）。別に起こした `emacs -Q` の `test/integration.el` で 56 項目 OK / NG 0

---

## 1. 作ったもの

| 場所 | 変更 |
|---|---|
| `crates/vt/src/ffi.rs` | `GhosttyClipboardContent` / `…WriteReply` / `…Write` / `…ReadReply` / `…Read`、`GhosttyTerminalDesktopNotification`、`GhosttyTerminalProgressReport`、コールバックの型、`OPT_COLOR_PALETTE` / `DEFAULT_CURSOR_STYLE` / `DEFAULT_CURSOR_BLINK` / `CLIPBOARD_WRITE` / `CLIPBOARD_READ` / `DESKTOP_NOTIFICATION` / `PROGRESS_REPORT`、`DATA_COLOR_PALETTE_DEFAULT`、`ghostty_grid_ref_hyperlink_uri`。配置と列挙の値は `tests.rs` で `ghostty_type_json()` と照合した |
| `crates/vt/src/lib.rs` | `Effects` に `clipboard_write`（受け入れたら true）・`clipboard_read`・`desktop_notification`・`progress`。`ProgressState`。`Terminal::set_clipboard_read_enabled`（**読み取りのコールバックは許したときだけ入れる**）、`set_default_palette16`（組み込みの 256 色を最初に読んで覚え、16 色だけ差し替える）、`set_default_cursor`、`hyperlink_uri` |
| `crates/session/src/lib.rs` | `Event` に `ClipboardWrite` / `Notification` / `Progress`（中身を載せるので `Copy` をやめた）。`SessionOptions` に `colors: Colors`・`cursor: CursorDefault`・`clipboard_write` / `clipboard_read`。`Session::set_colors` / `set_cursor_default` / `set_clipboard_policy`。読み取りを許したときのクリップボードは Win32 で直接読む（`OpenClipboard` を 2 ms おきに 5 回まで。待ち続けない） |
| `crates/render/src/frame.rs` | パレットが変わったら全部の行を写し直す（セルの色は写すときに解決済みのため）。`cursor.blinking` の変化も描き直しの理由にする |
| `crates/render/src/lib.rs` | `Renderer::draw_blink`（点滅の 1 回分）、`cursor_cell`、`draw_cursor` を分けた、`draw_rows` に行の範囲、`DrawStats`（全体 / カーソルのセルだけの回数） |
| `apps/embterm-module/src/term.rs` | `Request::SetColors` / `SetCursor` / `SetClipboardPolicy` / `LinkAt` / `DebugInfo`、`Event::Clipboard` / `Notification` / `Progress` / `Link`。点滅のタイマー（`TIMER_CURSOR_BLINK`）、Ctrl+クリックのリンク、リンクの上で手のカーソル、アプリ由来の知らせのまとめ（`emit_from_app`） |
| `apps/embterm-module/src/lib.rs` | `embterm-module-set-colors` / `-set-cursor` / `-set-clipboard-policy` / `-link-at` / `-debug-info`（後ろ 2 つは自動確認用）。`poll` の `:events` に `(clipboard . TEXT)` `(notification TITLE . BODY)` `(progress STATE . PERCENT)` `(link . URI)` |
| `apps/embterm/src/window.rs` | 新しい `SessionOptions` に合わせただけ（embterm.exe は OSC 52 を拒む既定のまま） |
| `lisp/embterm.el` | 下の §2 のコマンドと設定。`embterm` を「あれば切り替える」に変えた |
| `test/integration.el`（新規）・`test/osc-child.py`（新規） | 自動確認（§3）。`osc-child.py` は OSC 52（書き込み・読み取り）/ 9 / 777 / 9;4 / 8、BEL の連打、任意の列（DECSCUSR・OSC 11 など）を出す子 |
| `test/post-mouse.ps1` | `cdown` / `cup` / `cmove`（Ctrl を押した扱い） |
| `test/{smoke,split,keys,kitty,activate}.el` | `(embterm)` → `(embterm-new)`（`embterm` が既存の端末へ切り替えるようになったため） |

## 2. Lisp の使い方

| 名前 | 中身 |
|---|---|
| `embterm` | 端末があれば切り替える（いま端末にいれば、ほかの端末のうち最近選んだもの）。無ければ `default-directory` で開く。`C-u` で必ず新規 |
| `embterm-new` | 必ず新しく開く |
| `embterm-project` | `project-current` のルートで開く。同じプロジェクトの端末（`embterm--project` が同じ）があれば使い回す。`C-u` で新規 |
| `embterm-list-buffers` | `completing-read` で端末を選ぶ（注釈はディレクトリ） |
| `embterm-sync-colors` | 色を送り直す（`set-face-attribute` で face を直接変えたとき。テーマの切り替えは自動） |
| `embterm-follow-theme`（t） | `default` の前景・背景、`cursor` の背景、`region` の背景と前景、`ansi-color-black`〜`ansi-color-bright-white` の前景。`enable-theme-functions` / `disable-theme-functions` で全端末へ |
| `embterm-color-foreground` / `-background` / `-cursor` / `-selection` / `-selection-foreground` / `-palette` | 追従しないときの色（nil は module の既定） |
| `embterm-cursor-style`（`block`）/ `embterm-cursor-blink`（nil） | 既定の形と点滅。アプリの DECSCUSR が優先 |
| `embterm-allow-clipboard-write`（t）/ `embterm-allow-clipboard-read`（nil） | OSC 52 |
| `embterm-notification-functions` | 既定は `embterm-notification-message`（端末のウィンドウが選ばれていないか、フレームにフォーカスが無いときだけ `embterm 通知 [バッファ名] タイトル: 本文` を `message`） |
| `embterm-progress-functions`（nil） | OSC 9;4。引数 STATE PERCENT BUFFER |
| `embterm-hyperlink-function`（`embterm-open-hyperlink`）/ `embterm-hyperlink-schemes` | Ctrl+クリックしたリンクを開く。http / https / ftp / mailto は `browse-url`、`file:` は `find-file-other-window`、ほかは開かない |
| `embterm-bell-function`（`ding`、引数 t で呼ぶ）/ `embterm-bell-interval`（0.2 秒） | ベル |
| `embterm-buffer-name-function`（`embterm-default-buffer-name`）/ `embterm-buffer-name-max-length`（40） | バッファ名 |

### 2.1 判断したこと

- **カーソルの既定**: libghostty-vt は「アプリが DECSCUSR で決めていない間は、既定（`DEFAULT_CURSOR_STYLE` / `_BLINK`）を今のカーソルにも使い、決めたあとはアプリの形のまま、`CSI 0 q` で既定へ戻る」を自分で持っている（`tests.rs` の `default_cursor_style_and_decscusr`）。なので EmbTerm は既定を渡すだけ
- **既定の色**: `COLOR_FOREGROUND` などと `COLOR_PALETTE` は「既定」を変え、OSC 10 / 11 / 12 / 4 の上書きは残る（同 `default_palette_keeps_osc4_overrides`、`emacs -Q` でも OSC 11 の背景がテーマを替えても残り、OSC 111 で戻ることを確認）。パレットは 256 色を渡す API なので、組み込みの 256 色を最初に読んで覚え、16 色だけ差し替える
- **色は子プロセスの起動前に渡す**: `embterm--create` が `embterm-module-create` の直後に色・カーソル・許可の頼みごとを積む。頼みごとは順に片付くので、最初の place（起動）より前に `SessionOptions` に入る（起動直後の OSC 11 の問い合わせにもテーマの色で答える）
- **点滅はカーソルのセルだけ**: フリップモデルの 2 枚のバッファは交互に出るので、全体を描いたあとの 1 回目の点滅は全体を描いて 2 枚をそろえ、以後はクリップの中でその行だけを描く（はみ出したグリフも正しく重なる）。全体を描いたら（出力・リサイズ・デバイスの作り直し）またそろえ直す。画像（Kitty graphics）があるときは全体を描く。タイマーは**選んでいるペインが見えていて、下を追っていて、フォーカスがあり、カーソルが見えて点滅するとき**だけ動く。間隔は `GetCaretBlinkTime`（Windows の設定。点滅しない設定なら点滅しない）。カーソルが動いたとき・入力したときは点けてやり直す
- **OSC 52 の読み取りは既定で拒む**: コールバックを入れない（libghostty-vt は OSC 52 の `?` を無視し、OSC 5522 の読み取りを EPERM で断る）。`embterm-allow-clipboard-read` のときだけ入れて、Windows のクリップボードを読みのスレッドで直接読む（コールバックは同期で、Lisp を待てないため）
- **OSC 52 の書き込み**も可否は module で判断する（読みのスレッドで同期で答えるので `AtomicBool`）。Lisp は受けたものを `kill-new`（同じテキストが先頭にあれば入れない）
- **知らせのまとめ**: まだ Lisp が読んでいないベルは 1 つに、進捗は最後のものに置き換え、知らせが 1000 を超えたら捨てる（アプリが大量に出しても Emacs が詰まらない）。Lisp もベルを `embterm-bell-interval` で間引く
- **リンク**: Ctrl+クリック（マウスの報告が有効なアプリの中でも Ctrl ならリンクが優先）。`file:` は `browse-url` に渡すと Windows で `ShellExecute` され実行ファイルが起動しうるので、Emacs で開く（claude が出すファイルのリンクにも合う）。マウスの形は、リンクの上で手（報告中は Ctrl を押しているときだけ）
- **バッファ名**: タイトルが `C:\…` や `/…` で始まるパスならファイル名（拡張子なし）。cmd がコマンドの間に付ける ` - コマンド` は残す。タイトルが無ければ argv の先頭のファイル名。`embterm-buffer-name-format` に入れて全体を 40 に収める（`…`）。`rename-buffer` の UNIQUE はそのまま
- **tr-ime**: `tr-ime-modadv--reconvertstring-hook` / `--documentfeed-hook` をバッファローカルに `(embterm--tr-ime-notify-empty)` だけにする（t を入れないのでグローバルは呼ばれない）。`tr-ime-modadv--notify-reconvert-string` に `""` と 0 を渡すので、C++ 側は待たず、再変換で `delete-char` も走らない（`subclass_proc.cc` の `get_reconvert_string` / `set_reconvert_string` を読んで確かめた）。変数が無ければ何もしない

## 3. 確かめたこと

`cargo build`・`cargo test --workspace`（et-vt 25 件、うち新規 5 件）・`cargo clippy --workspace --all-targets` は警告なし、`embterm.el` の byte-compile も警告なし。
`test/smoke.el`・`split.el`（NG 0）・`keys.el`・`kitty.el`（NG 0）・`activate.el` は完走。

`C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/integration.el`（約 65 秒）: **OK 56 / NG 0**（`target/t034/integration.log`）。

| 確かめたこと | 結果 | 画面 |
|---|---|---|
| `emacs -Q` の既定（白地）に追従 | 背景 `#ffffff`・前景 `#000000` | `C:\Projects\EmbTerm\target\t034\frame-theme-default.png` |
| `load-theme 'modus-vivendi` | 背景 `#000000`、赤 = `ansi-color-red`（`#ff5f59`）、16 色が modus の色 | `C:\Projects\EmbTerm\target\t034\frame-theme-vivendi.png` |
| アプリが OSC 11 で背景を変えたあと `modus-operandi` へ | 背景は `#203040` のまま、前景は operandi に。OSC 111 で operandi の背景へ | `C:\Projects\EmbTerm\target\t034\frame-theme-operandi.png` |
| `embterm-cursor-style` = bar | 縦棒 | `C:\Projects\EmbTerm\target\t034\frame-cursor-bar.png` |
| アプリの DECSCUSR 4 → 設定を block に → DECSCUSR 0 | 下線のまま → 下線のまま → ブロック | |
| `embterm-cursor-blink` = t（3 秒） | 点滅 6 回、**全体の描画 0 回・カーソルのセルだけ 6 回**。バッファを隠すとタイマーが止まり回数が増えない。nil に戻すとタイマーが止まりカーソルは点いている | `C:\Projects\EmbTerm\target\t034\frame-cursor-blink.png` |
| OSC 52 の書き込み | kill-ring に入る。`embterm-allow-clipboard-write` = nil なら入らない | |
| OSC 52 の読み取り | 既定では答えない（子の入力に何も来ない）。許すと `\e]52;c;<base64>\a` が子に届く（`target/t034/clip-read-*.log`） | |
| OSC 9 / 777 | hook が `("" "osc9 body text" BUF)` と `("Claude Code" "Claude is waiting for your input" BUF)` で呼ばれる。端末を選んでいなければ `embterm 通知 [*embterm: cmd*] Claude Code: waiting` を出す | |
| OSC 9;4 | hook が `(set 50)` と `(remove nil)` で呼ばれる | |
| ベル 100 回を約 1 秒で | `embterm-bell-function` は 5 回 | |
| OSC 8 | リンクのセルの URI が取れる。Ctrl なしのクリックとリンクでないセルの Ctrl+クリックでは開かず、リンクの Ctrl+クリックで `browse-url` が 1 回（`browse-url-browser-function` を差し替え。ブラウザは開いていない）。`javascript:` は開かない | `C:\Projects\EmbTerm\target\t034\frame-link.png` |
| `embterm-new` / `embterm` / `C-u embterm` / `embterm-project`（`test/` と `crates/` から）/ `embterm-list-buffers` | 新規 / ほかの端末へ / 新規 / ルート `c:/Projects/EmbTerm/` で開き、2 回目は使い回す / 選んだ端末へ | `C:\Projects\EmbTerm\target\t034\frame-end.png` |
| バッファ名 | `C:\WINDOWS\SYSTEM32\cmd.exe` → `*embterm: cmd*`、`… cmd.exe - python  x.py` → `*embterm: cmd - python  x.py*`、タイトル無し → `*embterm: pwsh*`、100 文字 → 幅 40、作った直後から `*embterm: cmd*` | |
| tr-ime（フックを作って単体で） | 端末のバッファでは `("" 0)` が 2 回（再変換・前後参照）通知され、グローバルのフックは呼ばれず、中身も変わらない。ほかのバッファではグローバルが動く。tr-ime が無いときは変数を作らない | |

## 4. 未解決・見ていないこと

- **リンクの上の手のカーソルは自動確認していない**（`WM_SETCURSOR` は本物のマウスの位置を読むので `PostMessage` では起こせない）。手で確かめる必要がある
- **tr-ime は本物の環境で確かめていない**（`emacs -Q` に無いので、フック関数を直接呼ぶ単体の確認だけ）。ユーザーの Emacs で、端末のバッファで再変換キー（変換キー）を押しても何も起きず、待たされないことを確かめる。端末を開いたあとで tr-ime を読み込んだ場合は差し替わらない（ふつうは init で先に読む）
- `embterm-follow-theme` は選んでいるフレームの face を使う（フレームごとの face の違いは見ない）
- リンクに下線やホバーの強調は描いていない（アプリが付けた下線だけ）。リンクの URI をエコーエリアなどに出すこともしていない
- 通知は `message` だけ（Windows のトースト通知は出さない）。進捗の既定の表示も無い
- embterm.exe（デバッグ用）は点滅も色の設定も持たない（module だけ）
- 点滅の間隔は Windows の設定（`GetCaretBlinkTime`）で、Emacs の `blink-cursor-interval` とは別

## 5. TODO に足すべき課題

- 通知を Windows のトースト（または `w32-notification-notify`）で出す選択肢。claude の「入力待ち」をフレームが前面にないときに気づけるように
- 進捗（OSC 9;4）をモードラインに出す既定の関数（Windows Terminal のタブの進捗のような表示）
- OSC 8 のリンクのホバー表示（下線・URI のエコー）。現状は手のカーソルだけ
- 配色の報告（`COLOR_SCHEME` のコールバック、`CSI ?996n` / mode 2031）。テーマを替えたときにアプリへダーク / ライトを知らせる
- `embterm-project` のバッファ名にプロジェクト名を入れるか（今はタイトルかコマンド名なので、プロジェクトの端末が見分けにくい）
- ユーザーの環境での手の確認: tr-ime の再変換キー、リンクの手のカーソル、claude の OSC 9 / 777 の通知と OSC 52（`/copy`）、150% モニタでの点滅のセルの位置
