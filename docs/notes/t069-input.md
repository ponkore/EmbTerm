# T-069 キーの送り方（KKP / win32-input-mode / レガシー）、T-070 / T-071 / T-074、T-038 の残り

- 日付: 2026-11-11
- 結論: **キーは Windows Terminal と同じ順で KKP → win32-input-mode → レガシーの VT に切り替えて送る。** Emacs のイベント（案 A）から
  仮想キー・スキャンコード・文字・修飾キーを作り直して `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _` を送り、コンソールのアプリ・PSReadLine・VT のアプリ・KKP のアプリの
  どれにも期待どおり届くことを、別起動の `emacs -Q` と記録用の子プロセスで確かめた。同期出力・端末の応答（CPR・セルの画素）・XTVERSION・マウスの報告も実測した

---

## 1. 作ったもの

| 場所 | 中身 |
|---|---|
| `crates/session/src/keys.rs` | **[`KeyEvent`]**（仮想キー・スキャンコード・文字・修飾キー。`from_emacs` / `from_char` / `from_win32`）、送り方ごとの変換（[`encode`]）。win32-input-mode は自前。文字は `VkKeyScanExW`、スキャンコードは `MapVirtualKeyExW`、`Uc` は `ToUnicodeEx` |
| `crates/session/src/lib.rs` | `CSI ?9001h/l` の走査（`Win32ModeScanner`）、`Session::key_protocol`・`encode_key`、KKP の安全策（`reset_stale_kkp`）、XTVERSION の設定 |
| `crates/pty` | `descendant_pids` / `ProcessWatch`（KKP を求めたアプリが落ちたかの判定） |
| `crates/vt` | `KeyEncoder::encode_input`（KKP）、`Terminal::kitty_keyboard_flags`・`set_xtversion`・`render_hold`・`take_held_render_state`（T-074）、`version()`、`Key::F` を F24 まで |
| `apps/embterm-module/src/term.rs` | キーの送り方を `KeyEvent` に、hold の間は写さない（全ペイン）、マウスの報告のドラッグ（全ボタン）、代替画面のホイール、マウスの形（`WM_SETCURSOR`） |
| `apps/embterm/src/window.rs` | `WM_KEYDOWN` / `WM_KEYUP` から `KeyEvent::from_win32`（本物の仮想キー・スキャンコード、離したときも送る）、hold、マウスの報告、代替画面のホイール、マウスの形 |
| `lisp/embterm.el` | `embterm--event-key` を `event-basic-type` + `event-modifiers` に統一、`C-SPC`・`C-S-<英字>`・`C-<数字記号>`・F1〜F24・テンキーの束縛を追加、`self-insert` は 1 文字の「キー」として送る |
| `test/key-child.py` | 記録用の子（console / vt / vt-kkp / query / syncout / drop-kkp / mouse / altwheel） |
| `test/keys.el` | 別起動の `emacs -Q` から送る自動確認。`KEYS_ONLY=drop,vim` で一部だけ走らせられる |

### 判断したこと

- **`?9001h` は自前の走査で知る。** libghostty-vt は未知のモードを黙って無視し、`UNKNOWN_SEQUENCE` は APC と OSC だけ（`terminal.h`）。読みのスレッドで DEC プライベートモードの列を走査する（途中で切れた列に対応）
- **win32-input-mode の押した / 離した**: Emacs から来たキーは離したイベントが無いので、`Kd=1` と `Kd=0` を続けて送る（`KeyPhase::Tap`）。embterm.exe は `WM_KEYUP` で本物の離したイベントを送る
- **`Uc` は `ToUnicodeEx` に任せる**: Ctrl+A = 1、Ctrl+Enter = 0x0a、Ctrl+Backspace = 0x7f、Ctrl+Space = 0x20 など、本物のキーと同じ値になる（実測の表）
- **IME の確定文字・絵文字**は `Vk = 0`（`VK_PACKET` 相当）+ `Uc`、1 文字（UTF-16 の 1 単位）ずつ。**実測で問題なし**（コンソールのアプリには `KEY_EVENT` として、VT 入力のアプリには UTF-8 で届き、PSReadLine にもそのまま入る）。Emacs から文字を送るときは `self-insert` も `embterm--self-insert` で「キー」として送る（貼り付け `C-y`・`embterm-send-string` は素の文字列のまま）
- **KKP の残りの安全策**:
  - 代替画面: libghostty-vt の KKP のスタックは画面ごと（主画面と代替画面で別々。ユニットテストで確認。Windows Terminal 1.25.3131 の「代替画面で KKP のモードを保つ」と同じ形）なので、代替画面を抜ければ自動で主画面のフラグに戻る
  - 主画面で求めたまま子が落ちた場合: **KKP が有効になった瞬間の最初の子（シェル）の子孫プロセス**（親子関係で求める。Job の一覧は pwsh の子が Job から抜けて使えなかった）を覚えておき、キーを送るときにそれがすべて終わっていればフラグを `CSI <99u` で戻す（`Session::reset_stale_kkp`）
  - **安全策なしで実測した（`if true { return; }` で切った）**: pwsh に `[eeeee...` のゴミ文字が入り、`ParserError` になった。**安全策ありで解消した**（結果の表）
- **マウス**: Windows の報告はクライアント座標（`renderer.padding()` 込みで `et_vt::MouseEncoder` が桁に直す）。ドラッグは左以外のボタンにも対応
- **代替画面のホイール（報告なし）**: Ghostty と同じく DEC 1007（`mouse_alternate_scroll`。既定で有効）なら矢印キーを 1 ノッチにつき 3 回送る（レガシーの形。conhost が仮想キーに直す）
- **マウスの形**: 報告が有効（Shift なし）なら矢印、それ以外（テキストの上）は I ビーム（`WM_SETCURSOR`）
- **T-071**: XTVERSION は `EmbTerm 0.1.0 (libghostty-vt 1.0.0)`。DA1 は libghostty-vt の既定 `CSI ?62;22c`（VT220 + ANSI 色。28 と 4 は名乗らない）のままで十分（`CSI c` の応答を実測）。`TERM_PROGRAM=EmbTerm` は yazi の環境変数の判定に当たらず、XTVERSION の `ghostty` でブランドが決まるので矛盾しない（conpty.md §4.1）
- **T-074**: hold の始まりで画面を写す（`cb_render_hold`）。写した render state は端末の dirty を受け取るので、UI は次の更新でそれを受け取って置き換える（`take_held_render_state`）。時限 1 秒はタイマーで起こす

---

## 2. 確かめたこと

`C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/keys.el`（結果は `target/t069/`）。`cargo test --workspace`（29 件）と `cargo clippy --workspace --all-targets` は警告なし。`smoke.el` 完走、`split.el` は NG 0。embterm.el の byte-compile も警告なし。**claude は起動していない**（ユーザーのアカウントを使わないため）。

### 各キーの結果（Emacs → 子）

| 送ったキー | console（`KEY_EVENT`） | vt（VT 入力の生） | vt-kkp（KKP） |
|---|---|---|---|
| `C-SPC` | `vk=0x20 sc=0x39 ch=0x20 Ctrl` | `\0` | `CSI 32;5u` |
| `S-<return>` | `vk=0x0d ch=0x0d Shift` | `\r` | `CSI 13;2u` |
| `C-<return>` | `vk=0x0d ch=0x0a Ctrl` | `\n` | `CSI 13;5u` |
| `M-a` | `vk=0x41 ch=0x61 Alt` | `ESC a` | `CSI 97;3u` |
| `<f5>` | `vk=0x74 sc=0x3f` | `CSI 15~` | `CSI 15~` |
| `C-<f5>` | `vk=0x74 Ctrl` | `CSI 15;5~` | `CSI 15;5~` |
| `<up>` | `vk=0x26 sc=0x48 ENHANCED` | `CSI A` | `CSI A` |
| `C-<left>` | `vk=0x25 Ctrl ENHANCED` | `CSI 1;5D` | `CSI 1;5D` |
| `a` | `vk=0x41 ch=0x61` | `a` | `a` |
| `A` | `vk=0x41 ch=0x41 Shift` | `A` | `A` |
| `C-S-a` | `vk=0x41 ch=1 Ctrl+Shift` | `\x01` | `CSI 97;6u` |
| `あ` | `vk=0 sc=0 ch=0x3042`（押した + 離した） | `あ` | `あ` |
| 絵文字 `😀` | `vk=0`、サロゲートの 2 単位が順に届く | `😀`（conhost が合成） | `😀` |
| `<tab>` / `<backtab>` | `vk=0x09` / `Shift` | `\t` / `CSI Z` | `\t` / `CSI 9;2u` |
| `<kp-1>` | `vk=0x61 ch=0x31` | `1` | `CSI 57400::49u` |

各キーで `KEY_EVENT` は押した（1）と離した（0）の 2 つが届く（console）。全 35 キーの結果は `target/t069/merged.txt`

### そのほかの実測

| 確認 | 結果 |
|---|---|
| PSReadLine（`Set-PSReadLineKeyHandler -Chord 'Ctrl+Spacebar'`）が `C-SPC` を受ける | OK（ハンドラが動いた） |
| PSReadLine に `あ😀※` を確定文字で入れて `echo` | OK（`psrl-alive-あ😀※` と表示） |
| `CSI 6n` | `\x1b[2;1R`（2 ms 以内） |
| `CSI 16t` | `\x1b[6;18;8t`（セル 8x18 px） |
| `CSI 18t` | `\x1b[8;29;79t` |
| XTVERSION | `\x1bP>\|EmbTerm 0.1.0 (libghostty-vt 1.0.0)\x1b\\` |
| DA1 | `\x1b[?62;22c` |
| フォーカス `CSI I` / `CSI O` | conhost 経由でコンソールのアプリに `FOCUS_EVENT` が届く |
| KKP を出した子が落ちた後の `C-SPC` + `echo`（安全策あり） | `echo KKP-ok` が実行され、ゴミ文字なし。送り方は `win32`、フラグ 0 |
| 同じ（安全策なし） | `[eeeee... KKP-ok` と `ParserError`（再現） |
| 同期出力（`?2026h` の間に partial を書く） | 71 サンプルのどれにも途中の表示は無し（`partial-9` が揃ってから出た） |
| マウス（`CSI ?1002;1006h` の子 + 子 HWND に PostMessage） | クリック・ドラッグ・離したが SGR で届く（`CSI <0;4;2M` など） |
| 代替画面のホイール（報告なし） | 上 1 ノッチで `CSI A` ×3、下 2 ノッチで `CSI B` ×6 |
| vim（scoop の vim）`:set mouse=a` のクリック | カーソルがクリック位置へ動いた（`getpos` で確認）。vim の画面の表示もスクリーンショットで確認 |
| embterm.exe（`WM_KEYDOWN` / `WM_KEYUP` / `WM_CHAR` を PostMessage） | console で press + release、スキャンコードも本物の値。vt で `a` `\x1bOP` `あ`。mouse・ホイールも同じ。`--system-conpty`（OS 同梱）も console で同じく OK |

### 見えていること（embterm.el / module の動き）

- `C-g` は Emacs の quit に当たる（既存）。`C-[` は `ESC` の接頭辞なので束縛していない（`M-` 系のキーマップが壊れるため）
- `C-m`（整数 13）は Emacs のふつうの `RET` として届くので Ctrl+M（`vk=M`）のまま送る。実キーの Enter は `<return>` イベントで届く

---

## 3. 未解決

- **マウスの形（`WM_SETCURSOR`）は目視していない。** 別プロセスから確かめる手段が無かった
- **`C-あ` `M-あ`（非 ASCII + 修飾）は束縛されない**ので端末に届かない（`embterm-semi-char-map` の束縛が ASCII の範囲のため）。必要になれば足す
- **IME の本物の確定** はこの自動確認では叩いていない（Emacs の文字イベントの経路で代用。ユーザーの手元での `あ`・絵文字の入力を確かめたい）
- 修飾キーの実測: `embterm.exe` の `Ctrl` / `Alt` 付きは PostMessage で作れない（`GetKeyboardState` は本物の状態）。修飾なしの押した + 離したまで確認。修飾付きの exe の経路はコードの共通部分（`KeyEvent::win32`）に任せている
- `vim`（scoop）の画面の `🔄` が `��` になる（vim のコンソール API の書き込みによる。EmbTerm 側の問題ではない見込み）

---

## 4. TODO に足す候補

- 非 ASCII の文字 + 修飾キー（`C-あ` など）を送るか決める
- KKP の安全策の限界: 求めたアプリの子孫が（別のシェルに手放されるなどで）長生きすると戻せない。`CSI <99u` は「今の画面」だけ。代替画面で落ちて別のアプリが続くと代替画面のフラグが残る
- pwsh の native command が Job から抜ける件（T-073）は今回の調べで再確認（プロセスの木でしか追えない）
- 実機の IME の確定文字（`あ` と絵文字）・`AltGr`・JP 配列での `C-@` などを手元で確かめる
- マウスの形の目視、代替画面のホイールを vim 以外（less、yazi）で確かめる
