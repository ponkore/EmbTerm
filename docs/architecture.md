# EmbTerm アーキテクチャ

- 作成日: 2026-11-16（T-060）
- 入力: [requirements.md](requirements.md)。決定の経緯は [TODO.md](TODO.md)、1 件ごとの記録は [notes/](notes/)、
  調査は [research/](research/)
- ステータス: **実装済みの構造を書いたもの**（検討の文書ではない）。Windows が本流、macOS は T-121 で移植した

この文書は「どこに何があり、どう繋がっているか」を書く。**なぜそうしたか**は [adr/](adr/)（§16）、
**使い方**は [../README.md](../README.md)。

---

## 1. 全体像

EmbTerm は **Emacs の dynamic module**（`embterm_module.dll`）が本体で、Emacs のウィンドウの本文領域に
**module が作った子ウィンドウ（HWND）を重ね、そこに端末の画面を直接描く**。
Emacs のバッファの中身は置き物で、画面のテキストではない。

![EmbTerm の全体像](images/architecture-overview.svg)

図は Windows のもの。**macOS はスレッドの分け方と頼みごとの渡し方が違う**（UI スレッドを作らない）ので、
別の図を §12 に置いてある。

## 2. 構成

| 置き場所 | クレート / ファイル | 中身 |
|---|---|---|
| `crates/vt` | `et-vt` | libghostty-vt（C ABI）の FFI と安全なラッパ。`Terminal`・`RenderState`・`KeyEncoder`・Kitty graphics。unsafe をここに閉じ込める |
| `crates/pty` | `et-pty` | 子プロセスを PTY で動かす。Windows は ConPTY + Job Object、Unix は `openpty` + `setsid` |
| `crates/session` | `et-session` | 端末 1 つ分の VT と PTY のつなぎ。読み / 書き / 終了待ちのスレッド、キーとマウスの変換、OSC のイベント化 |
| `crates/render` | `et-render` | 画面を描く。`Frame`（render state の写し）・`FrameView`（ペインの切り取り）・`Theme`。Windows は `d2d`、macOS は `mac` |
| `apps/embterm-module` | `embterm-module` | Emacs の dynamic module。`term.rs`（Windows の UI スレッドと子 HWND）/ `term_mac.rs`（macOS のレイヤーと描画スレッド）、`lib.rs`（Lisp から見える関数） |
| `apps/embterm` | `embterm` | 単体デバッグ用の exe。描画・VT・PTY は module と同じコードを使い、トップレベルのウィンドウで動く |
| `lisp/embterm.el` | — | Lisp 側。コマンド・キーマップ・設定・ペインの置き場所・module の知らせの受け口 |
| `etc/embterm.{ps1,bash,zsh,lua}` | — | シェル統合（OSC 133 のプロンプトの境界と OSC 51;E の `embterm_cmd`） |
| `xtask/` | `xtask` | 開発用のタスク（依存の取得・配布物づくり・回帰テスト） |
| `vendor/` | — | 取ってきた依存（libghostty-vt の静的ライブラリ、再配布版 ConPTY）。git の管理外 |
| `test/` | — | Emacs を別に起こして動かす自動確認（`*.el`）と、子プロセスの役をする台本（`*.py`） |

`spikes/` は初期の実験。今は使っていない。

## 3. プロセスとスレッド

Emacs のプロセスの中で動く。**端末 1 つにつき**次のスレッドが立つ。

| スレッド | 仕事 | 約束 |
|---|---|---|
| Lisp（Emacs のメイン） | `embterm.el`。頼みごとを積んで `PostMessage` するだけ | **止めない。** module の関数は待たない（例外はデバッグ用の `embterm-module-text` / `pane-text`） |
| UI | 制御用ウィンドウ（`HWND_MESSAGE`）とペインの子 HWND を持つ。頼みごとの処理と描画 | **重い処理をしない**（入力キューが Emacs と連結するので、詰まると Emacs のキーまで止まる）。`Present` で止まらない（T-068） |
| PTY の読み | 読む → 端末をロック → `vt_write` → ロックを外す → 描き直しの知らせ | 知らせは `AtomicBool` でまとめて 1 回だけ |
| PTY の書き | チャネルで受けて PTY に書く（端末の応答もここへ） | **ロックを持ったまま PTY に書かない**（相手が詰まると固まる） |
| session-close | 閉じるときに子の終了を待つ | 待つのはここだけ（T-076） |

- **module の別スレッドから Lisp を呼ばない。**
- 端末の状態（`Session`）は表示（HWND）より長生き（T-066）。親のフレームが消えて子 HWND が消えても、
  端末と UI スレッドは残り、次に置くときに作り直す。
- GPU デバイスはプロセスで 1 つを分け合う（T-067）。失ったら作り直し、ほかの端末にも知らせる。
- module の境界（`#[defun]`・wndproc・スレッドの本体・`Drop`）は `catch_unwind` で包む（T-031）。

## 4. Lisp ↔ module の境界

```text
Lisp → module : 頼みごとを Shared.requests に積む → PostMessage(制御用ウィンドウ) → UI スレッドが片付ける
module → Lisp : Shared.state / events に書く → open_channel のパイプに 1 バイト → Lisp が embterm-module-poll
```

- **行きは待たない。** モーダルループ（リサイズのドラッグ中など）で取りこぼさないよう、
  スレッド宛てではなく HWND 宛ての `PostMessage` を使う。
- **帰りはポーリング。** CRT を静的リンクした module では `open_channel` の fd を自分の CRT で引けないので、
  Emacs の CRT の `_get_osfhandle` を実行時に引いて書く（T-104。[adr/0007](adr/0007-threads-and-polling.md)）。
  知らせが使えない環境では `embterm-poll-interval`（既定 50 ms）のポーリングに落ちる。
- `embterm-module-poll` は世代（generation）を渡し、変わっていなければ何も返さない。
  返すのは plist（タイトル・カレントディレクトリ・ペインの状態・イベントの列）。

## 5. 表示

### 5.1 ペイン（Emacs のウィンドウ 1 つ = 子 HWND 1 枚）

- `window-size-change-functions` / `window-buffer-change-functions` / `window-selection-change-functions` で、
  端末のバッファを出している**すべてのウィンドウ**に子 HWND を置く（`window-inside-pixel-edges`)。
- `C-x 2` / `C-x 3` で分けたそれぞれが**別のスクロール位置**を持てる（T-016 / T-063）。
  libghostty-vt のビューポートは 1 つなので、描くときはロックの中で
  「そのペインの位置へ動かす → そのペインの render state に写す → いちばん下へ戻す」とする。
- 端末の大きさ（桁・行）は ConPTY に 1 つだけなので、**選んでいるペイン**に合わせる。
  ほかのペインは自分の大きさで切り取って描く。
- 子 HWND は `HWND_BOTTOM` に置いて上げない（corfu などの子フレームを隠さないため）。
  `WS_EX_NOACTIVATE` と `MA_NOACTIVATE` でフォーカスを取らない。
- 別のフレームに同じ端末を出したときは、今のフレームのウィンドウにだけ置く。

### 5.2 描く中身と描く条件

- 1 フレームの流れ: 端末をロック → `RenderState::update`（変わった行だけ写す）→ ロックを外す → 描く。
- 文字は色ごとに画面全体でまとめ、背景は**セル 1 つを 1 画素にしたビットマップ**を引き伸ばして描く。
- 描くのは変わったときだけ。ただし**端末の中身ではないもの**が変わったときも描き直す（T-133）:
  カーソルのフォーカス・IME の変換中の文字列（macOS）・マウスの下のリンクの下線・ペインの置き場所・
  テーマ・スクロール位置。
- フレームレートは垂直同期まで。スワップチェーンが次のフレームを受け付けるまで描くのを後回しにし、
  メッセージのループがそのハンドルも一緒に待つ（UI スレッドを `Present` で止めない）。

## 6. 入力

### 6.1 キー

**フォーカスは Emacs に残す**（[adr/0004](adr/0004-keys-in-emacs.md)）。子 HWND はフォーカスを取らない。

```text
キーボード
  └▶ Emacs のイベント（フォーカスはフレームにある。子 HWND は取らない）
       └▶ embterm-semi-char-map（emulation-mode-map-alists の先頭に置く）
            ├ embterm-keymap-exceptions のキー … Emacs のコマンドとして実行（C-x、M-x、…）
            └ それ以外 … embterm--send-event / embterm--self-insert
                 └▶ embterm-module-send-key（キーの名前 + 修飾キーの文字列）※ 待たない
                      └▶ 列に積んで PostMessage → UI スレッドが片付ける
                           └▶ et-session の keys.rs が KeyEvent に揃える
                                （VkKeyScanW / MapVirtualKeyW で仮想キー・スキャンコードを作り直す）
                                └▶ Session::key_protocol が送り方を選ぶ
                                     └▶ PTY へ書く（書きのスレッド）
```

| 送り方 | いつ | 作るもの |
|---|---|---|
| KKP（Kitty keyboard protocol） | アプリが `CSI > u` で求めたとき | libghostty-vt のエンコーダ |
| win32-input-mode | ConPTY が立てているとき | `CSI Vk;Sc;Uc;Kd;Cs;Rc _`（`KEY_EVENT_RECORD` そのもの）を自前で |
| レガシーの VT | それ以外 | 文字は UTF-8、Ctrl は制御文字、Alt は ESC 前置。特殊キーは libghostty-vt のエンコーダ |

- `embterm-keymap-exceptions`（既定 `C-c` `C-x` `C-u` `C-h` `M-x` `M-:` `C-\`）は Emacs に残す。
- キーボードで打てない文字（IME の確定文字・絵文字）は、仮想キー 0 + UTF-16 の 1 単位ずつで送る。
- 貼り付けなどの長い文字列は win32-input-mode にしない（1 文字 2 列で重い）。素の文字列で送る。
- copy モード（`C-c C-k`）では端末を隠し、スクロールバックごとバッファに写してキーを Emacs に返す。

### 6.2 IME

- **Windows**: 変換中の文字列を描くのは IME 自身。位置は tr-ime advanced の
  `tr-ime-modadv--set-composition-window` で上書きする（[adr/0005](adr/0005-ime.md)）。
  入れた位置は `WM_IME_STARTCOMPOSITION` が来たときだけ IME に渡るので、module から同じメッセージを投げて
  その場で反映させ、確定した文字は送った幅だけ先読みする（T-137）。
- **macOS**: `ns-put-working-text` / `ns-unput-working-text` に advice を掛けて変換中の文字列を受け取り、
  **module が端末のカーソルの位置に自分で描く**。候補ウィンドウの位置は `EmacsView` の
  `firstRectForCharacterRange:` を差し替えて返す（T-127 / T-129 / T-133）。

### 6.3 マウス

- Windows は子 HWND が直接受ける。macOS はレイヤーが当たり判定に出ないので Emacs のイベントを Lisp が送る。
- アプリがマウスを見ていれば報告（SGR）を送り、そうでなければ選択に使う。
  `Shift` を押している間は、報告が有効なアプリでも選択にする。
- ホイールは ①報告が有効なら報告 ②代替画面 + DEC 1007 なら矢印 ③それ以外は端末のスクロールバック（T-136）。

## 7. 端末の状態

- VT の解釈は libghostty-vt（[adr/0001](adr/0001-libghostty-vt.md)）。端末はスレッド安全ではないので `Mutex` で守る。
- スクロール位置と選択は `TrackedRef` / libghostty の選択で持つので、出力が増えても同じ行を指す。
- 画面サイズが変わったら再折り返し（reflow）して ConPTY にも伝える。
  縦に広げてもスクロールバックの行は画面に戻さない（ConPTY の画面と食い違うため）。
- 画像（Kitty graphics）は、どこに何を描くかを render state から、画素は端末のロックの中から読む。
  PNG のデコードは Windows が WIC、macOS が ImageIO。

## 8. PTY と子プロセス

- **再配布版の ConPTY（`conpty.dll` + `OpenConsole.exe`）を同梱して優先**し、無ければ OS 同梱を使う
  （[adr/0003](adr/0003-conpty.md)）。
- 子の環境は Emacs の `process-environment` から作る（`setenv` が届く）。
  `TERM=xterm-256color`・`COLORTERM=truecolor`・`TERM_PROGRAM=EmbTerm`・`TERM_PROGRAM_VERSION` を足し、
  `PWD` / `OLDPWD` と、親が入れた他の端末の印（`WEZTERM_*`・`WT_SESSION` など）は落とす。
- 子孫は Job Object に入れる。閉じるときは `CTRL_CLOSE_EVENT` → 猶予 → Job ごと止める。
  Emacs が異常終了しても子孫と `OpenConsole.exe` は残らない（T-073 / T-076）。

## 9. 文字幅とフォント

- **eaw（East Asian Ambiguous）は 1 桁で固定**（[adr/0006](adr/0006-eaw-width.md)）。
  Emacs の `char-width-table` とは無関係に端末側で決める。
- 1 桁と数えた文字のグリフが全角のとき（`①` `★` `※`）の描き方は `embterm-narrow-wide` で選ぶ:
  **overlap**（既定。全角のまま描いて桁送りは 1 セル）/ **shrink**（1 セルに縮める）。
- フォントのフォールバックは「主フォント → `embterm-font-fallback-families` → OS のフォールバック」。
  **合字はしない**（セルごとに描く）。`text-scale-adjust` がバッファごとに効く。

## 10. OSC とシェル統合

| OSC | 扱い |
|---|---|
| 0 / 2（タイトル） | バッファ名に入れる（`embterm-buffer-name-function`） |
| 7（カレントディレクトリ） | `default-directory` を追わせる |
| 8（ハイパーリンク） | 下線を引き、マウスの下で URI をエコー領域に、`Ctrl`+クリックで開く |
| 9 / 777（通知） | `embterm-notification-functions` |
| 9;4（進捗） | モードラインと `embterm-progress-functions` |
| 52（クリップボード） | 書き込みは kill-ring へ。読み取りは既定で拒む |
| 133（プロンプトの境界） | `C-c C-p` / `C-c C-n` で前後のプロンプトへ、`C-c C-o` で直前の出力をコピー、終了コードをモードラインに |
| 51;E（Emacs の関数を呼ぶ） | `embterm-shell-commands` に載っているものだけ呼ぶ。シェル側は `etc/embterm.*` の `embterm_cmd` |

OSC 133 と 51;E を出すのは `etc/embterm.{ps1,bash,zsh,lua}`。端末の中でだけ読み込む（`TERM_PROGRAM` で判定）。

## 11. 設定

設定はすべて elisp から（`M-x customize-group RET embterm`）。
module は Lisp から呼べる関数と知らせを出すだけで、設定画面や拡張言語は持たない。
色は `embterm-follow-theme` が non-nil なら Emacs のテーマ（`default` `cursor` `region` と `ansi-color-*`）に追従し、
`enable-theme-functions` / `disable-theme-functions` で開いている端末に反映する。

## 12. OS による違い

| | Windows | macOS |
|---|---|---|
| スレッド | 端末ごとに **UI スレッド 1 本**（頼みごとも描画もここ） | **UI スレッドを作らない。** 頼みごとは main スレッドでその場、描画だけ別のスレッド |
| Lisp → module | 列に積んで HWND 宛ての `PostMessage`（待たない） | `MacApp`（`thread_local`）をその場で触る（AppKit は main スレッドだけ） |
| 埋め込み | フレームの HWND を親にした子 HWND | `EmacsView` のレイヤーに `CAMetalLayer` を足す（当たり判定に出ない） |
| 描画 | DXGI のフリップモデル + Direct2D / DirectWrite | Metal + CoreText のグリフアトラス |
| PTY | ConPTY（再配布版を優先）+ Job Object | `openpty` + `setsid`、プロセスグループに SIGHUP |
| キー | 仮想キー・スキャンコードを作り直して 3 通り | Emacs のキーの名前から（win32-input-mode は無い） |
| IME | tr-ime で変換ウィンドウの位置を上書き | 変換中の文字列を module が自分で描く |
| マウス | 子 HWND が受ける | Emacs のイベントを Lisp が送る |
| フレームの引き方 | `window-id`（HWND） | z 順 : フレームの幅 : 高さ（`window-id` は使えない） |

スレッドと頼みごとの渡し方が変わるので、§1 の図とは骨格が違う。macOS の全体像は次のとおり。

![EmbTerm の全体像（macOS）](images/architecture-overview-macos.svg)

`et-session` のスレッド（PTY の読み書き）と libghostty-vt は共通で、
**違うのは「誰が頼みごとを片づけ、誰が描くか」と「どこに描くか」**になる。

Linux と `emacs -nw` は対象外。

## 13. 配布と更新

- 配布物は `cargo xtask dist` が `target/dist/` に集める:
  `embterm_module.dll`・`embterm.el`・`embterm.exe`・`conpty.dll`・`x64/OpenConsole.exe`・`etc/embterm.*`。
- 読み込み済みの DLL は差し替えられないので、`embterm.el` は **module を一時ディレクトリに写してから読む**（T-030）。
  新しい版を使うには Emacs の再起動が要る。
- **普段使いの Emacs に `target/dist` を直接読ませない**（`conpty.dll` と `OpenConsole.exe` がロックされ、
  次の `dist` と試験が上書きに失敗する）。別の場所に写して読ませる（T-120）。

## 14. 試験

| 道具 | 中身 |
|---|---|
| `cargo test` | クレートの単体テスト（VT の解釈・キーの変換・レンダラの画素・レイアウト） |
| `cargo xtask test-screens` | 画面の回帰テスト。`embterm.exe` でケースを動かし、画面のテキストを期待値と比べる（vttest を含む） |
| `cargo xtask test-emacs` | 別に起こした `emacs -Q` で `test/*.el` を動かし、終了コードとログの `NG` を見る |

Emacs の自動確認は実際にフレームを開き、子プロセス（`test/*.py`）に届いたバイト列やログで判定する。

## 15. 参照

| | |
|---|---|
| [requirements.md](requirements.md) | 要件（確定・決定・案の別） |
| [TODO.md](TODO.md) | 決定の経緯（T-番号。T-138 で凍結。いまの課題は GitHub Issues） |
| [notes/](notes/) | 1 件ごとの作業の記録 |
| [research/](research/) | 調査（ConPTY、Emacs の w32 IME、libghostty-vt、macOS 移植、ghostel） |
| [../README.md](../README.md) | 使い方・インストール |

## 16. 決定の記録（ADR）

| | |
|---|---|
| [0001](adr/0001-libghostty-vt.md) | VT の解釈に libghostty-vt を使う |
| [0002](adr/0002-child-hwnd-direct-draw.md) | 子 HWND を重ねて自前で描く（Emacs のバッファに描かない） |
| [0003](adr/0003-conpty.md) | PTY は ConPTY。再配布版を同梱して優先する |
| [0004](adr/0004-keys-in-emacs.md) | フォーカスは Emacs に残し、キーは Emacs のキーマップで受けて送る |
| [0005](adr/0005-ime.md) | IME の変換ウィンドウは tr-ime の上書きで端末のカーソルに合わせる |
| [0006](adr/0006-eaw-width.md) | eaw は 1 桁で固定し、広いグリフの描き方を選べるようにする |
| [0007](adr/0007-threads-and-polling.md) | Lisp → module は `PostMessage`、module → Lisp はポーリング（+ パイプの知らせ） |
| [0008](adr/0008-panes.md) | 1 つの端末を Emacs のウィンドウごとのペインで描く |
