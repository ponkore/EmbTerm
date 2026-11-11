# ghostel の実装を読む

- 日付: 2026-11-11
- **TODO T-019 の記録**
- 対象: ghostel 0.62.0（commit `91c86d2`、2026-11-09）、依存は libghostty-vt 1.0.0（`build.zig.zon`）
- ソースの場所: `C:\Users\masao\.emacs.d\straight\repos\ghostel`。以下の `src/…` `lisp/…` はここからの相対パス
- 導入時の実測（ビルド済み module の配布、ConPTY の経路の概略、eaw の桁ずれが起きないこと、既知の制限）は
  `~/.emacs.d/docs/pty/ghostel.md` にある。**ここでは繰り返さず、参照だけする**

**凡例**

| 書き方 | 意味 |
|---|---|
| ソースより | ghostel のソース（または git の履歴）を読んで分かったこと。動かしてはいない |
| 実測 | `~/.emacs.d/docs/pty/ghostel.md` に記録のある実測 |
| 推測 | 根拠が間接的なもの。T-020 などで確かめる |

---

## 0. 要点

1. **ghostel の現行コードは libghostty-vt の C ABI を使っていない。Zig の API（`@import("ghostty-vt")`）を使う。**
   2026-05-20 に C API から Zig API へ移った（commit `0b4e8cd`）。そのため現行コードの関数名は
   EmbTerm（Rust から C ABI を呼ぶ）にはそのまま使えない。C API の名前は移行前のコード
   （ghostty `01825411`、**1.0 より前**）から拾った（§2.3）。**1.0 のヘッダとの照合は T-020**
2. 移行の理由の一つは、**1.0 前の C API が OSC 7 / 52 / 133 などを捨てていた**ことで、ghostel は
   生のバイト列を自前で走査して拾っていた（§2.4）。**EmbTerm が C ABI で OSC を受け取れるかは
   T-005 の最重要の確認点**
3. **文字幅（1 桁 / 2 桁）は libghostty-vt が決める。** ghostel は幅を一切設定していない。
   eaw の扱いを変える設定も ghostel には無い（§3）
4. ConPTY は「再配布版 `conpty.dll` → 無ければ kernel32」「overlapped の名前付きパイプ」
   「`STARTF_USESTDHANDLES` に NULL」「`ClosePseudoConsole` を別スレッドで」など、**そのまま流用できる
   作法が揃っている。** ただし **Job Object を使っておらず、孫プロセスは止めない**（§4）
5. **端末ごとに読みのスレッドが 1 本あり、そのスレッドが VT の解釈まで行う。** Lisp への通知は
   `open_channel` のパイプに **Lisp の式をテキストで書き**、Emacs 側が `read`→`eval` する（§5）
6. **静的リンクの CRT でも `open_channel` を使える回避策がある。** Emacs 本体が `_dup` を import している
   CRT（msvcrt / ucrtbase）を PE の import 表から探し、その CRT の `_write` / `_close` を呼ぶ
   （`src/ConPtyProcess.zig:78-219`）。GenPreview ADR 0006 が「正しい直し方」と書いて見送った方法そのもの（§5.3）
7. キーは **フォーカスを Emacs に残し、Emacs のキーマップで受けてから libghostty-vt のキーエンコーダで
   符号化する**（T-009 の案 A）。IME は Emacs に任せきりで、Windows の IME に固有の処理は無い（§6）

---

## 1. 全体の形（ソースより）

```text
Emacs（Lisp スレッド）                               読みのスレッド（端末ごとに 1 本、detach）
──────────────────────                               ─────────────────────────────────
ghostel--send-event → ghostel--encode-key ──WriteFile──▶ ConPTY 入力パイプ
ghostel--redraw（タイマー）                                ConPTY 出力パイプ ──ReadFile(64 KiB)──┐
  └ 端末をロックしてバッファへ差分を書く  ◀──ロック──▶  端末をロックして VT を解釈（nextSlice）◀─┘
ghostel--events-filter ◀── パイプ（open_channel）── コールバックを Lisp の式にして書く、最後に "()"
  └ eval → ghostel--invalidate → タイマー
```

| ファイル | 役割 |
|---|---|
| `src/module.zig` | `emacs_module_init`、PNG デコードのフック、ログ |
| `src/GhostelTerm.zig` | 端末 1 つ（`gt.Terminal` + `gt.Stream` + Renderer）と Lisp に出す関数 |
| `src/handler.zig` | `gt.Stream` のハンドラ。標準ハンドラに委ねつつ OSC などを横取りする |
| `src/Renderer.zig` | 画面とスクロールバックを Emacs バッファのテキストとして書く |
| `src/input.zig` | Emacs のキー名 → `gt.input.KeyEvent` |
| `src/kitty_graphics.zig` | Kitty graphics の配置を Lisp に渡す |
| `src/NativeProcess.zig` | 読みのスレッド、PTY への書き込み、Lisp への通知（OS 共通部分） |
| `src/ConPtyProcess.zig` | ConPTY の実装（Windows） |
| `lisp/ghostel.el` | モード・キーマップ・タイマー・プロセスの管理 |

---

## 2. libghostty-vt の API の使い方

### 2.1 前提: Zig API と C API（ソースより）

- `build.zig:36-43` で `ghostty_dep.module("ghostty-vt")` を import している。`emit-lib-vt = true` は付けるが、
  リンクしているのは Zig のモジュールであって C のライブラリではない
- `git log` の `0b4e8cd`（2026-05-20）「Switch from libghostty-vt C API to Zig API」。移行前は
  `src/ghostty.zig` で `@cInclude("ghostty/vt.h")` していた（`0b4e8cd^:src/ghostty.zig:1-6`）
- 移行前の依存は ghostty の commit `01825411`（`1.3.2-dev`）。いまの依存は `libghostty-vt-1.0.0.tar.gz`
  （`build.zig.zon`、commit `30cb6ca`、2026-11-07）。**つまり C API の名前は 1.0 前のもので、1.0 で変わった可能性がある**
- **libghostty-vt のヘッダは手元に無い。** `%LOCALAPPDATA%\zig` は存在せず、
  `~/.emacs.d/tmp/libghostty-1.0.0-probe` は空だった
- 現行コードは `gt.PageList` / `gt.Pin` / `page.getCells` などの**内部構造を直接触っている**。
  C ABI からは同じことはできない（推測）。型が export されていないという TODO が 3 か所ある
  （`src/Renderer.zig:427`、`src/style_face.zig:26`、`src/handler.zig:155`）

### 2.2 使っている機能の一覧

「現行（Zig）」は 0.62.0 のコード。「旧 C API」は `0b4e8cd^` のコードで使っていた名前（1.0 前）。

| 機能 | 現行（Zig） | 旧 C API（1.0 前） | 備考 |
|---|---|---|---|
| 端末の生成 | `gt.Terminal.init(io, alloc, Options)`（`src/GhostelTerm.zig:48-78`）。Options は `rows` `cols` `max_scrollback_bytes`（既定 5 MiB）`kitty_image_storage_limit`（既定 320 MiB）`kitty_image_loading_limits`（`src/GhostelTerm.zig:492-524`） | `ghostty_terminal_new(alloc, &t, {cols, rows, max_scrollback})` + `ghostty_terminal_set(GHOSTTY_TERMINAL_OPT_*)` | スクロールバックは**バイト数**で制限。行数は `setScrollbackMaxLines`（`src/GhostelTerm.zig:187`） |
| 既定のモード | `opts.default_modes = .{ .grapheme_cluster = true }`（`src/GhostelTerm.zig:57`） | `ghostty_mode_new(2027, false)` + `ghostty_terminal_mode_set`（`0b4e8cd^:src/terminal.zig:73`） | **mode 2027（書記素クラスタ）を最初から ON** |
| バイト列の投入 | `gt.Stream(Handler).nextSlice(data)`（`src/GhostelTerm.zig:139-144`、読みのスレッドでは `src/NativeProcess.zig:49-58`） | `ghostty_terminal_vt_write(t, ptr, len)` | 分割された入力をそのまま渡してよい |
| ホストへの応答（DA・DSR などの返事） | ハンドラの `effects.write_pty`（`src/handler.zig:44,130-134`）→ PTY に書く | `GHOSTTY_TERMINAL_OPT_WRITE_PTY` + `OPT_USERDATA` | §2.5 |
| ベル | `effects.bell`（`src/handler.zig:45,143-146`） | `GHOSTTY_TERMINAL_OPT_BELL` | |
| タイトル（OSC 0/2） | `effects.title_changed` で通知、`terminal.getTitle()` で読む（`src/handler.zig:48,195-206`） | `OPT_TITLE_CHANGED` + `GHOSTTY_TERMINAL_DATA_TITLE` | RIS では通知が来ないので自前で補う（`src/handler.zig:86-94`） |
| タイトルのスタック（CSI 22/23 t） | ghostel が自前で持つ（`src/handler.zig:114-115,212-239`） | — | libghostty 側は未対応らしい（コメントより） |
| DA1/DA2/DA3 | `effects.device_attributes` で VT220 + ANSI color を返す（`src/handler.zig:166-181`） | `OPT_DEVICE_ATTRIBUTES` | XTVERSION は現行では設定していない（旧 C は `OPT_XTVERSION` を定義だけ） |
| サイズの報告（CSI 14/16/18 t） | `effects.size`（`src/handler.zig:184-191`） | `OPT_SIZE` | セルのピクセルは 1 以上に丸める（`src/GhostelTerm.zig:612-625`。0 だと Kitty graphics 非対応と見なすアプリがある） |
| 配色の問い合わせ（CSI ? 996 n / mode 2031） | `effects.color_scheme`（`src/handler.zig:149-153`）、変化時に `gt.device_status.encodeColorSchemeReport`（`src/GhostelTerm.zig:793-809`） | — | |
| 既定の前景・背景・パレット（OSC 10/11/4 の返事に使う） | `terminal.colors.{foreground,background}.default`、`colors.palette.changeDefault`（`src/GhostelTerm.zig:134-137,531-532,796-797`） | `OPT_COLOR_FOREGROUND/BACKGROUND/PALETTE` | |
| 同期出力（DEC 2026） | `effects.render_hold`（`src/handler.zig:50,137-140`）+ `modes.get(.synchronized_output)`（`src/Renderer.zig:161`） | — | §2.6 |
| OSC 133（プロンプト境界） | `effects.semantic_prompt`（`src/handler.zig:51,248-261`）、セルの `semantic_content` | — | |
| OSC 7501（プログラムの状態） | `effects.program_status`（`src/handler.zig:53,268-289`） | — | 登録すると libghostty が対応の問い合わせに答える |
| OSC 7 / OSC 9;9（cwd） | `vt()` で `.report_pwd` を横取り（`src/handler.zig:81,308-311`） | **C API では捨てられていた**（§2.4） | |
| OSC 52（クリップボード） | `.clipboard_contents` を横取り（`src/handler.zig:82,324-335`） | 同上 | 問い合わせ `?` は無視。`e` は ghostel 独自の elisp 実行 |
| OSC 9 / 777（通知） | `.show_desktop_notification`（`src/handler.zig:83,345-348`） | 同上 | 空の通知は捨てる |
| OSC 9;4（進捗） | `.progress_report`（`src/handler.zig:84,355-365`） | 同上 | |
| OSC 8（ハイパーリンク） | セルの `hyperlink` → `page.lookupHyperlink` / `hyperlink_set`（`src/Renderer.zig:305-318,439`） | `ghostty_grid_ref_hyperlink_uri` | 明示 id と暗黙 id を区別して保持 |
| tmux control mode（DCS 1000 p） | `dcs_hook/put/unhook` を横取り（`src/handler.zig:96-111`） | — | EmbTerm には不要 |
| 画面の読み出し | `screens.active.pages` を `Pin.rowIterator(.right_down)` で回し（`src/Renderer.zig:972`）、`page.getCells(row)`（579）、`cell.codepoint()` / `hasGrapheme()` / `lookupGrapheme()`（601-605）、`cell.wide`（583）、`style_id` → `page.styles.get`（412）、背景だけのセル `content_tag`（440-448） | `ghostty_render_state_new/update`、`GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR`、`ghostty_render_state_row_iterator_next`、`ghostty_render_state_row_cells_next`、`GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_{RAW,STYLE,FG_COLOR,BG_COLOR,GRAPHEMES_BUF,GRAPHEMES_LEN}`、`ghostty_cell_get(GHOSTTY_CELL_DATA_WIDE …)` | §2.3 |
| dirty | 行の `row.dirty`、ページの `page.dirty`、`Pin.isDirty()`。描いたら自分で落とす（`src/Renderer.zig:943-955,1015,208-211`）。**カーソルが動いた行は自前で dirty 扱い**（949-952） | `GHOSTTY_RENDER_STATE_DATA_DIRTY`（`FALSE/PARTIAL/FULL`）、`GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY`、描いたら `ROW_OPTION_DIRTY` / `OPTION_DIRTY` を false に `set`（`0b4e8cd^:src/Renderer.zig:891-930`） | |
| カーソル | `screen.cursor.x/y`、`cursor_style`、`modes.get(.cursor_visible / .cursor_blinking)`（`src/Renderer.zig:1024-1042`） | `GHOSTTY_RENDER_STATE_DATA_CURSOR_{VISIBLE,BLINKING,VISUAL_STYLE,VIEWPORT_X,VIEWPORT_Y,VIEWPORT_HAS_VALUE}` | 形は BLOCK / BLOCK_HOLLOW / BAR / UNDERLINE |
| スクロールバック | 描き終えた位置を `pages.trackPin` で覚え（`src/Renderer.zig:106,1090`）、そこから下だけ描く。代替画面は `no_scrollback`。消えた行は `evictScrollback`（1097-1117） | `GHOSTTY_TERMINAL_DATA_SCROLLBAR` を見て、`ghostty_terminal_scroll_viewport(TOP/BOTTOM/DELTA)` で viewport を「駐車」させて取り出していた（`0b4e8cd^:src/Renderer.zig:145` 付近） | **C API の render state は viewport しか見せない**ため、スクロールバックの取り出しが回りくどかった |
| resize と reflow | `terminal.resize(alloc, {cols, rows, cell_size_px})`（`src/Renderer.zig:1054-1062`）。**resize は同期出力モードを消すので保存して戻す**（1053,1062） | `ghostty_terminal_resize` | reflow は libghostty 側で行う（推測。ghostel は列が変わるとバッファを消して描き直すだけ、1069-1074） |
| 全文のテキスト | `gt.formatter.TerminalFormatter`（`.plain`、`unwrap`、`trim`）（`src/GhostelTerm.zig:914-928`） | `ghostty_formatter_terminal_new` / `format_alloc` | |
| selection | **使っていない**（選択は Emacs のバッファの上で行う） | — | |
| search | **使っていない**（isearch を使う） | — | |
| キーのエンコード | `gt.input.KeyEncodeOptions.fromTerminal(&terminal)` + `gt.input.encodeKey`（`src/GhostelTerm.zig:220-243`） | `ghostty_key_encoder_new` / `ghostty_key_encoder_setopt_from_terminal` / `ghostty_key_encoder_encode`、`ghostty_key_event_new` / `set_action` / `set_key` / `set_mods` / `set_utf8` | §6。DECCKM・Kitty の flags などは端末から読む |
| マウスのエンコード | `gt.input.MouseEncodeOptions.fromTerminal(&terminal, {screen, cell = 1x1})` + `encodeMouse`（`src/GhostelTerm.zig:245-282`） | `ghostty_mouse_encoder_new` / `setopt_from_terminal` / `setopt(GHOSTTY_MOUSE_ENCODER_OPT_SIZE)` / `encode`、`ghostty_mouse_event_*` | セルを 1x1 ピクセルとして渡す |
| フォーカス | `modes.get(.focus_event)` + `gt.input.encodeFocus`（`src/GhostelTerm.zig:284-300`） | `ghostty_focus_encode` | |
| 貼り付け | `gt.input.PasteOptions.fromTerminal` + `encodePaste`（`src/GhostelTerm.zig:302-317`） | — | **1 回の write で送る**（zsh の KEYTIMEOUT 対策のコメント） |
| モードの問い合わせ | `modes.get(mode)`、`gt.modes.modeFromInt`（`src/GhostelTerm.zig:842-864`）、代替画面 `screens.active_key == .alternate`（878）、マウス追跡 `flags.mouse_event`（896） | `ghostty_terminal_mode_get`、`GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING` | |
| Kitty graphics | `screens.active.kitty_images` の `placements`、`placement.location`（`.pin` / `.relative` / `.virtual`）、`pixelSize` / `gridSize` / `sourceRect`、`image.renderData()`。Unicode placeholder は自前で走査（`src/kitty_graphics.zig`） | `ghostty_kitty_graphics_get`、`ghostty_kitty_graphics_placement_iterator_new/next`、`ghostty_kitty_graphics_placement_render_info`、`ghostty_kitty_graphics_image_get_multi` | §2.7 |
| PNG のデコード | `gt.sys.decode_png = &png.decode`（`src/module.zig:59`。stb_image） | `ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG, …)` | **libghostty は PNG を自分でデコードしない。ホストが渡す** |
| ログ | `std_options.logFn`（`src/module.zig:132-175`） | `ghostty_sys_set(GHOSTTY_SYS_OPT_LOG, …)` | |
| スナップショット | `gt.snapshot.decodeExact`（`src/GhostelTerm.zig:168-209`） | — | desktop の復元用。EmbTerm には当面不要 |

### 2.3 旧 C API の読み出しの形（ソースより、1.0 前）

旧 `Renderer.render`（`0b4e8cd^:src/Renderer.zig:891-930`）は次の順だった。EmbTerm が C ABI で描くときの雛形になる。

1. `ghostty_render_state_update(state, terminal)` で端末の状態を render state に写す
2. `GHOSTTY_RENDER_STATE_DATA_DIRTY` を読む（`FALSE` / `PARTIAL` / `FULL`）
3. `GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR` で行の反復子を取り、`row_iterator_next` で回す
4. 行ごとに `ROW_DATA_DIRTY` を見て、dirty な行だけ `row_cells_next` でセルを回し、
   `ROW_CELLS_DATA_RAW`（セル本体。そこから `ghostty_cell_get` で WIDE / STYLE_ID / HAS_HYPERLINK など）、
   `STYLE`、`FG_COLOR` / `BG_COLOR`（解決済みの色）、`GRAPHEMES_*` を読む
5. 行の dirty を `ghostty_render_state_row_set(…, ROW_OPTION_DIRTY, false)`、最後に全体を `OPTION_DIRTY = FALSE`

**render state は「ある時点の viewport の写し」なので、写したあとは端末のロックを離して描ける**（推測。
EmbTerm で描画スレッドを分けるときに効く）。一方で viewport の外（スクロールバック）を読むには
`ghostty_terminal_scroll_viewport` で viewport を動かすか、`ghostty_terminal_grid_ref`（`GHOSTTY_POINT_TAG_SCREEN`）
でセルを個別に引く必要があった（`0b4e8cd^:src/module.zig:1323`）。

### 2.4 C API では OSC が取れなかった（ソースより）

移行前の `fnWriteInput`（`0b4e8cd^:src/module.zig:252-330`）は次のことをしていた。

- **「libghostty-vt が捨てる OSC（7, 51, 52, 133）」を、投入したバイト列から自前で走査して拾う**（同 320 行のコメント）
- OSC 4/10/11 の色の問い合わせには、**libghostty に渡す前に自前で返事を書く**（`extractOscColorQueries`）。
  同じ write の中の CSI 6n の返事より先に色の返事を出さないと、termenv 系のアプリが読み捨てるため
- commit `df7ea9b` のメッセージは「~395 行の並行スキャナ（OscIterator、色の返事の整形、OSC 9/777）を消して、
  ghostty が解釈済みの Command を直接読む」と書いている

**1.0 の C ABI で OSC 7 / 8 / 9 / 52 / 133 / 777 をコールバックで受け取れるかは未確認。** 受け取れないなら
EmbTerm も自前の OSC スキャナが要る（T-005 の判断材料）。

### 2.5 ホストへの応答の経路（ソースより）

- 返事は `nextSlice` の中から**同期的に** `write_pty` で呼ばれる。そのとき端末はロックされている
- 読みのスレッドでは `NativeProcess.ptyWriteFromTerminal`（`src/NativeProcess.zig:138-167`）が ConPTY の入力パイプへ書く。
  **待たない。** パイプが詰まっていたら `error.TerminalReplyWouldBlock` で**返事を捨てる**（`rejectTerminalReplyWait`、169-171）。
  ロックを持ったまま待つと、ロックを待つ Lisp スレッドと絡んで固まるため
- Lisp スレッドからの書き込み（キー入力）は `write_mutex` を `tryLock` で 20 ms ごとに試し、その間 `C-g` を見る
  （`src/NativeProcess.zig:112-136`）。WriteFile も 20 ms ごとに目覚めて `C-g` を見る（`src/ConPtyProcess.zig:401-418`）

### 2.6 同期出力（DEC 2026）（ソースより）

- 2026 が立っている間は描かない（`src/Renderer.zig:161`）
- ただし `render_hold(false)`（2026 が閉じた）が来たら `frame_completed = true` にして、**次の 1 回は描く**
  （`src/handler.zig:137-140`）。閉じてすぐ開き直すアプリが固まって見えないように（CHANGELOG 0.58.0）
- 強制的に描く経路もある（`ghostel--redraw` の FORCE-SYNC、`src/GhostelTerm.zig:632-650`）

### 2.7 Kitty graphics（ソースより）

- 配置は描くたびに libghostty の状態から全部読み直す（`src/kitty_graphics.zig:15-29`）。`.pin` は位置そのまま、
  `.relative` は親をたどる（`resolveChain`）、`.virtual` は Unicode placeholder（U+10EEEE）のセルを行ごとに走査する
- 画素は `image.renderData()`。PNG は送信時に `gt.sys.decode_png` で RGBA にされる。ghostel は PPM にして Emacs の画像にする
- ファイル・一時ファイル・共有メモリの媒体は**既定で無効**、引数で opt-in（`src/GhostelTerm.zig:486-523`）。
  リモートのプログラムに手元のファイルを読ませないため

---

## 3. 文字幅

### 3.1 誰が決めるか（ソースより）

- **libghostty-vt が文字を置くときに決め、セルの `wide`（`.narrow` / `.wide` / `.spacer_tail` / `.spacer_head`）に入れる。**
  ghostel はそれを読むだけ（`src/Renderer.zig:583,714`）
- ghostel が端末に渡す設定は `rows` `cols` スクロールバック Kitty の制限 `default_modes` だけで
  （`src/GhostelTerm.zig:57,492-524`）、**幅に関わる設定は一つも無い**
- ただし **mode 2027（書記素クラスタ）を既定で ON** にしている（`src/GhostelTerm.zig:57`）。絵文字の ZWJ 列や
  VS16 の幅はクラスタ単位で決まる。旧コードのコメントは「Emacs もそう描くので」（`0b4e8cd^:src/terminal.zig:72`）

### 3.2 East Asian Ambiguous

- 実測では `★` `※`（eaw）を **1 セル**として扱っている（`~/.emacs.d/docs/pty/ghostel.md`「eaw の桁ずれは起きない」）。
  つまり **libghostty-vt の既定は eaw = 1 桁**
- **ghostel には eaw を 2 桁にする設定が無い。** libghostty-vt 側に設定があるかはヘッダが無く未確認。
  推測では無い（ghostty 本体にも ambiguous width の設定は無かったはず）
- `①`（U+2460、eaw）は ghostel.md の表に無い。claude の桁と合うかは T-025 で測る
- ConPTY（conhost）は mode 2027 を知らない。**書記素クラスタの幅で conhost と libghostty が食い違う余地がある**（推測、T-011）

### 3.3 セルより広いグリフ（ソースより）

Emacs のバッファに描くための工夫で、EmbTerm が自前で描くなら同じ問題を描画側で解くことになる。

| 処理 | 場所 | 内容 |
|---|---|---|
| 対象の選び方 | `src/Renderer.zig:289-298,610-621` | 既定フォントが持たない最初のコードポイント（0xFF〜0x300 を `font-has-char-p` で探す）以上の文字と、書記素クラスタだけを調べる |
| 寸法の取得 | `src/Renderer.zig:814-889` | `font-shape-gstring` でグリフの送り幅・ascent・descent を取る。`GlyphMetricsCache` に保存 |
| 縮め方 | `src/Renderer.zig:699-754` | セル幅（1 桁か 2 桁）を `min-width` で固定し、幅・ascent・descent の比の最小で `height` を縮める。ascent と descent は**別々に**押さえる（行が高くならないように）。`ghostel-glyph-scale-floor`（既定 0.0）より小さくはしない |
| はみ出し | `src/Renderer.zig:756-812` | 1 桁の文字でも、**前後が空白で孤立しているときだけ**右隣の空白セルを借りて 2 桁の幅で描く（借りた空白は `(space :width 0)` で消す）。隣に文字があるときは借りない（見た目の大きさがばらつくため） |
| フォントに無い文字 | `src/Renderer.zig:881-884`（TODO） | 何もしない。豆腐が出て行が高くなることがある |

---

## 4. ConPTY（ソースより）

概略は ghostel.md「ConPTY の実装」にある。ここは EmbTerm が真似るための細部。

### 4.1 生成の手順

| 段 | 場所 | 内容 |
|---|---|---|
| 1. API の解決 | `src/ConPtyProcess.zig:470-502` | まず **module の DLL と同じディレクトリの `conpty.dll`** を `LoadLibraryW`（フルパス）し、`ConptyCreatePseudoConsole` / `ConptyResizePseudoConsole` / `ConptyClosePseudoConsole` を引く。無ければ `kernel32.dll` の `CreatePseudoConsole` 系 |
| 2. module のディレクトリ | `src/ConPtyProcess.zig:525-540` | `GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT)` に自分の静的変数のアドレスを渡し、`GetModuleFileNameW` |
| 3. `OpenConsole.exe` の置き場所 | `lisp/ghostel-module-install.el:123-138` | `conpty.dll` の隣の `x64\OpenConsole.exe` / `arm64\OpenConsole.exe`。探すのは `conpty.dll` 自身（推測） |
| 4. パイプ | `src/ConPtyProcess.zig:633-688` | `CreatePipe` ではなく**名前付きパイプ** `\\.\pipe\ghostel-conpty-{pid}-{乱数}`（`FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS`、バッファ 4096）。**module 側の端だけ `FILE_FLAG_OVERLAPPED`**（読みのキャンセルと待ち合わせのため） |
| 5. 擬似コンソール | `src/ConPtyProcess.zig:542-575` | `CreatePseudoConsole(size, in_read, out_write, flags = 0, &hpc)`。作ったら ConPTY 側の端（`in_read` `out_write`）は閉じる |
| 6. 子プロセス | `src/ConPtyProcess.zig:690-757` | `STARTUPINFOEXW` + `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`（0x20016）。**`STARTF_USESTDHANDLES` で hStd* を NULL** にする（709-715。Emacs が SSH などでリダイレクトされているとき、親の stdio を子が継ぐのを防ぐ。Microsoft Terminal のメンテナ推奨の回避策）。`CreateProcessW(EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT, bInheritHandles = FALSE)` |
| 7. コマンドライン | `src/ConPtyProcess.zig:759-815` | argv0 は常に `"…"` で囲み `/` を `\` に。argv0 に `"` があれば失敗。残りは MSVCRT の規則でクォート。Lisp 側で実行ファイルを絶対パスに解決してから渡す（`lisp/ghostel.el:4165-4167`） |

### 4.2 読み・書き・resize

| 項目 | 場所 | 内容 |
|---|---|---|
| 読み | `src/ConPtyProcess.zig:246-308,339-360` | overlapped の `ReadFile`（64 KiB）を出し、`WaitForMultipleObjects(読み完了, 子プロセスのハンドル, 中断イベント)`。読めたら `nextSlice` |
| 書き | `src/ConPtyProcess.zig:362-434` | overlapped の `WriteFile` を 4096 バイトずつ。中断イベントでも起きる |
| resize | `src/ConPtyProcess.zig:436-443` | `ResizePseudoConsole(hpc, {cols, rows})`。ピクセルは渡さない |
| resize の順番 | `src/GhostelTerm.zig:96-121`、`src/Renderer.zig:141-158,1044-1076` | Lisp の `ghostel--set-size` は「次の描画で反映」として保留し、ピクセル寸法だけ先に入れる。**次の `redraw` で `terminal.resize` → 描画 → 大きさが変わっていれば `ResizePseudoConsole`** |
| ConPTY 向けの設定 | `src/GhostelTerm.zig:349-351` | Windows では `flags.resize_pull_scrollback = false`。縦に広げたときスクロールバックの行を画面に戻さない。**ConPTY は自分の画面バッファ（スクロールバック無し）を持っているので、戻すと画面の中身が食い違う**（CHANGELOG 0.58.0「後の出力が違う行に出た」） |

### 4.3 終了の検知と止め方

| 項目 | 場所 | 内容 |
|---|---|---|
| 終了の検知 | `src/ConPtyProcess.zig:246-253` | **子プロセスのハンドルを読みの待ち合わせに入れておく。** 子が終わると読みのループを抜ける |
| 後始末 | `src/ConPtyProcess.zig:255-261,601-626` | `ClosePseudoConsole` を**別スレッド**で呼び、読みのスレッドは EOF まで読み続けてから join する。古い Windows では `ClosePseudoConsole` が出力を吐き切るまで戻らないため |
| 終了コード | `src/ConPtyProcess.zig:459-468`、`src/NativeProcess.zig:350-362` | 子を `WaitForSingleObject(INFINITE)` で待ち、終了コードを**数値だけの行**としてパイプに書いて閉じる。Lisp 側はそれを見てパイプを消し、sentinel を走らせる（`lisp/ghostel.el:3896-3903,4221-4236`） |
| バッファを消したとき | `lisp/ghostel.el:4266-4278`、`src/NativeProcess.zig:364-370` | `ghostel--kill-native-process` → 中断イベントを立てるだけ。**子は殺さない**（docstring「This does not send SIGKILL」、`src/GhostelTerm.zig:1022-1041`）。擬似コンソールを閉じて子の終了を待つ |
| 強制終了 | `src/ConPtyProcess.zig:57-63`、`lisp/ghostel.el:4221-4236,4258-4264` | sentinel と `kill-emacs-hook` で `TerminateProcess(子, 9)`。**直接の子だけ** |
| 孫プロセス | — | **Job Object を使っていない。** 擬似コンソールを閉じると conhost が接続中のコンソールプロセスを終わらせるので多くは止まる（推測）が、コンソールから離れた孫（GUI、`start` で出したもの）は残る。また子が終わらないと読みのスレッドは `INFINITE` で待ち続ける |

### 4.4 環境変数

| 変数 | 場所 | 値 |
|---|---|---|
| `TERM` ほか | `lisp/ghostel.el:4019-4052` | 既定は `TERM=xterm-ghostty`、`TERMINFO=<同梱の terminfo>`、`TERM_PROGRAM=ghostty`、`TERM_PROGRAM_VERSION=1.3.2`（固定値）、`COLORTERM=truecolor`。terminfo が無ければ `xterm-256color` |
| `INSIDE_EMACS` | `lisp/ghostel.el:4131` | `ghostel` |
| `EMACS_GHOSTEL_PATH` | `lisp/ghostel.el:4318` | シェル統合のスクリプトの場所 |
| `PWD` | `lisp/ghostel.el:4104-4112` | **Windows では渡さない**（MSYS のシェルが Windows 形式の値を OSC 7 にそのまま貼り付けるため） |
| 組み立て | `src/GhostelTerm.zig:428-460` | `process-environment` を逆順にたどって Map にする（先頭が勝つ）。`=` の無い要素は削除 |
| 文字コード | `src/platform.zig:49-71` | **unibyte の文字列はシステムの ANSI コードページとして UTF-8 に直す。** multibyte は UTF-8 のまま。引数・環境変数・cwd すべて（CHANGELOG 0.45.0） |

Windows で `TERM` を読むのは MSYS2（git bash）と Cygwin 系だけで、wsl には `WSLENV` を通さないと届かない（推測）。

---

## 5. スレッドと Emacs との境界（ソースより）

### 5.1 スレッドの分担

| スレッド | すること | 場所 |
|---|---|---|
| Lisp スレッド | module の関数すべて。**描画（Emacs バッファへの書き込み）**、キーの符号化と PTY への書き込み、resize、パレットの設定 | `src/GhostelTerm.zig:466-1081` |
| 読みのスレッド（端末ごとに 1 本、detach） | ConPTY の出力を読み、**端末をロックして VT を解釈**する。コールバックを Lisp の式に直してバッファに溜め、読み 1 回ごとにパイプへ流す | `src/NativeProcess.zig:107-108,259-308` |
| 閉じるスレッド（短命） | `ClosePseudoConsole` だけを呼ぶ | `src/ConPtyProcess.zig:601-626` |

VT の解釈用の `gt.Stream` は 2 つある。読みのスレッド用（ハンドラの Effects が `*NativeProcess`、`src/NativeProcess.zig:45,84`）と、
Lisp から `ghostel--write-vt` で流すとき用（Effects が `*GhostelTerm`、`src/GhostelTerm.zig:38,72`）。同じ `gt.Terminal` を指す。

### 5.2 Lisp への知らせ方

1. Lisp が `make-pipe-process` でパイプを作り（`lisp/ghostel.el:4238-4256`）、module が `open_channel` で fd を得る
   （`src/GhostelTerm.zig:975`、`src/emacs.zig:402-404`）
2. 読みのスレッドはコールバックを `(ghostel--set-title "…")` のような**Lisp の式のテキスト**にして 16 KiB のバッファに溜める
   （`src/NativeProcess.zig:209-257`）。読み 1 回ごとに、何も無くても `()` を書いて流す（319-322）
3. Lisp のフィルタ `ghostel--events-filter` が `read-from-string` で式を切り出し、バッファの中で `eval` する。
   最後に `ghostel--invalidate`（`lisp/ghostel.el:3876-3915`）
4. `ghostel--invalidate` が描画のタイマーを掛ける（`lisp/ghostel.el:4359-4392`）。

| 条件 | 遅れ | 場所 |
|---|---|---|
| 最後のキー入力から 50 ms 以内の出力（エコー） | **すぐ描く** | `ghostel-immediate-redraw-interval`（`lisp/ghostel.el:360-364`） |
| 100 ms 以上静かだったあとの最初の出力 | 16 ms | `lisp/ghostel.el:4375-4384` |
| 出力が続いているとき | 33 ms（約 30 fps） | `ghostel-timer-delay`（`lisp/ghostel.el:310-314`） |
| バッファが見えていない | 描かない（保留） | `lisp/ghostel.el:4389-4392` |

native PTY を使わないとき（Emacs のプロセス、TRAMP）は、コールバックの中から `ghostel--defer` → `run-at-time 0` で Lisp を呼ぶ
（`src/GhostelTerm.zig:211-218`、`lisp/ghostel.el:3459-3467`）。

### 5.3 静的 CRT でも `open_channel` に書ける回避策

`EventWriter`（`src/ConPtyProcess.zig:78-219`）がしていること。

1. `GetModuleHandleW(NULL)`（= emacs.exe）の **PE の import 表を読み**、`_dup` をどの DLL から import しているかを見る
   （`msvcrt.dll` → msvcrt、`ucrtbase.dll` / `api-ms-win-crt-*` → UCRT）
2. その CRT（`msvcrt.dll` または `ucrtbase.dll`）から `_write` と `_close` を `GetProcAddress` で引く
3. `open_channel` の fd には**その関数で**書く・閉じる

コメント（132-133 行）:「Emacs の `open_channel` が返すのは Emacs が持つ CRT の fd なので、Emacs が import している主系統の CRT で書く必要がある」。
GenPreview ADR 0006 の「Emacs が読み込んでいる CRT から関数を引いて呼ぶ」案と同じ考え方である。GenPreview が見送った理由は
「emacs クレートが生の `emacs_value` を公開しておらず `open_channel` を自前で呼べない」ことで、**CRT の問題そのものは解ける**。

### 5.4 ロック

| ロック | 種類 | 守るもの | 場所 |
|---|---|---|---|
| `GhostelTerm.mutex` | 再帰ロック（同じスレッドなら何度でも） | 端末の状態全部。読みのスレッドは解釈の間、Lisp は描画・符号化・設定の間持つ | `src/RecursiveMutex.zig`、`src/GhostelTerm.zig:34,328-334` |
| `NativeProcess.write_mutex` | 通常 | PTY への書き込みの順序（キー入力と端末の返事） | `src/NativeProcess.zig:36,112-167` |
| `backend_handoff_mutex` | 通常 | ConPTY の片付けと resize / 書き込みの競合 | `src/NativeProcess.zig:35,173-205,310-317` |

- **イベントのバッファがあふれたら、端末のロックを外してからパイプに書く**（`src/NativeProcess.zig:324-343`）。
  Emacs がパイプを読むには（フィルタから描画へ進むので）端末のロックが要ることがあり、持ったまま書くと固まる
- **Lisp スレッドの描画は端末のロックを持ったまま Emacs のバッファを書き換える。** その間、読みのスレッドは止まる
- 中断（`C-g`）は `should_quit` と `process_input` で見る（`src/emacs.zig:325-335`）

---

## 6. キー入力（ソースより）

### 6.1 経路

**フォーカスは Emacs に残る**（端末の画面がバッファそのものなので）。

```text
キーイベント → ghostel の入力モードのキーマップ
  ├ 文字（self-insert）  → ghostel--self-insert → UTF-8 のバイトをそのまま PTY へ（lisp/ghostel.el:1656-1665）
  └ それ以外             → ghostel--send-event → (KEY-NAME . "shift,ctrl,meta") に分解（1667-1722）
                           → ghostel--encode-key（module）→ input.keyEvent（src/input.zig:96-115）
                           → gt.input.encodeKey（端末のモードを反映）→ PTY
                           → 符号化できなければ Lisp で生のシーケンスを組む（lisp/ghostel.el:1556-1645）
```

- `KeyEncodeOptions.fromTerminal` が DECCKM・Kitty keyboard の flags などを端末から取る（`src/GhostelTerm.zig:225-233`）。
  `macos_option_as_alt = .true` で meta を常に alt として扱う
- 単一の ASCII 文字は「シフトしない codepoint」と「生成テキスト」を両方付ける。大文字は小文字 + shift（consumed）にする
  （`src/input.zig:96-115`）。Kitty の CSI u と alt の ESC 前置に両方要るため
- Kitty でのテスト: `M-t` → `ESC[116;3u`、`C-s` → `ESC[115;5u`（`src/input.zig:134-162`）。レガシーでは `C-m` も `ESC[109;5u`（fixterms）
  にして Enter と区別する（200-206）

### 6.2 Emacs に残すキー

| 入力モード | 端末に送るもの | 場所 |
|---|---|---|
| semi-char（既定） | `ghostel-keymap-exceptions`（既定 `C-c` `C-x` `C-u` `C-h` `M-x` `M-:` `C-\`）以外。`C-y` は ghostel の yank、`C-g` は別扱い、`C-SPC` は Emacs のマーク | `lisp/ghostel.el:555-563,1152-1240,1316-1350` |
| char | ほぼ全部（例外も送る）。抜けるのは `M-RET` / `C-M-m` | `lisp/ghostel.el:1352-1381` |
| copy / emacs / line | 送らない（Emacs のバッファとして操作） | — |

- RET / TAB / ESC の生の C0（端末版 Emacs で `C-m` などと区別できない）は機能キーとして扱い ctrl を落とす（`lisp/ghostel.el:1676-1683`）
- `C-@` は NUL。GUI の `C-SPC` は char モードだけ送る（1233-1240）

### 6.3 足りないところ

- **押したときだけ**送る（`.action = .press`、`src/input.zig:97`）。離した・リピートは無い。Kitty の「イベント種別を報告」は満たせない
- `mapKey` が知っているのは英数字・記号・矢印・Home/End/PgUp/PgDn/Ins/Del・F1〜F12 だけ（`src/input.zig:11-66`）。
  テンキー・F13 以上・メディアキーは `unidentified`
- 文字（self-insert）はエンコーダを通らない。Kitty の「すべてのキーをエスケープで報告」では文字も符号化すべきだが、しない
- **win32-input-mode（DECSET 9001）は扱っていない**（ソースに 9001 の記述が無い）。ConPTY には VT のシーケンスだけを送る

### 6.4 IME

- **Windows の IME に固有の処理は無い**（`w32-ime` / `tr-ime` への言及なし）。確定した文字列は文字のキーイベントとして
  届き、self-insert で UTF-8 のまま送られる。変換中の文字列は Emacs がバッファ（= 端末の画面）に描く
- `lisp/ghostel-ime.el` は **Emacs Lisp の入力メソッド（quail、hangul）用**。合成中は描画とバッファ編集の転送を止める
  （`ghostel-inhibit-redraw-functions`、`ghostel-inhibit-input-forwarding-functions`、`lisp/ghostel-ime.el:154-185`）
- バッファに直接挿入された文字（入力メソッドや yank）は `after-change-functions` で拾って PTY に送り、バッファからは消す。
  改行を含めばブラケットペースト（`lisp/ghostel.el:1765-1780`）

### 6.5 マウス・フォーカス

- Lisp が `posn` からセルの (col, row) を出し、`ghostel--mouse-event TERM ACTION BUTTON ROW COL MODS` を呼ぶ（`lisp/ghostel.el:2144-2200`）。
  module はセルを 1x1 ピクセルとして渡す（`src/GhostelTerm.zig:256-263`）。**SGR-Pixels（mode 1016）ではピクセルでなくセル番号が出る**（推測）
- ホイールは、アプリがマウスを追跡しているときだけ押下として送る。トラックパッドのピクセル量は 1 行分たまるごとに 1 回（2144-2178）
- フォーカスは `after-focus-change-function` などで「選ばれたウィンドウに見えているか」の変化を見て、mode 1004 のときだけ送る（3841-3856）

---

## 7. Windows 固有の制限・回避策（ソースより）

| 項目 | 場所 | 内容 |
|---|---|---|
| libghostty-vt のビルド | ghostel.md | Windows 用のビルド済みは無い。Zig 0.17 でソースから |
| `open_channel` と CRT | `src/ConPtyProcess.zig:78-219` | Emacs の CRT の `_write` / `_close` を探して使う（§5.3） |
| 再配布版 ConPTY | `src/ConPtyProcess.zig:470-523` | inbox の conhost は「入力への応答の遅れ」があるので、隣の `conpty.dll` を優先（473-477 のコメント） |
| 親の stdio の継承 | `src/ConPtyProcess.zig:709-715` | `STARTF_USESTDHANDLES` + NULL |
| `ClosePseudoConsole` が戻らない | `src/ConPtyProcess.zig:601-626` | 別スレッドで閉じ、読みは EOF まで続ける |
| resize でスクロールバックを戻さない | `src/GhostelTerm.zig:349-351` | ConPTY の画面と食い違うため |
| フォアグラウンドのプロセスグループ | `src/ConPtyProcess.zig:445-448` | ConPTY には無い。`null` |
| パスワード入力の検出 | `src/pty_utils.zig:28` | Windows では常に false |
| unibyte の文字列 | `src/platform.zig:49-71` | ANSI コードページとして変換 |
| OSC 7 のパス | `lisp/ghostel.el:3554` 以下、CHANGELOG 0.52.0 | MSYS の `/d/foo`、`/cygdrive/d/foo`、`kitty-shell-cwd://HOSTd:/…` を Windows のパスに直す |
| `PWD` | `lisp/ghostel.el:4104-4112` | Windows では渡さない |
| 端末の返事 | `src/NativeProcess.zig:138-171` | 入力パイプが詰まっていたら捨てる（OS 共通だが ConPTY の 4 KiB バッファでは起きやすい、推測） |
| TODO / FIXME | `src/Renderer.zig:427`、`src/style_face.zig:26`、`src/handler.zig:155`、`src/Renderer.zig:881` | 前の 3 つは「ghostty-vt が型を export していない」、最後は「フォントに無い文字」。**Windows に固有の TODO / FIXME は無い** |

---

## 8. 評価: EmbTerm が参考にできる点と、真似しないほうがよい点

### 8.1 そのまま参考にできる

| # | 点 | 理由 |
|---|---|---|
| 1 | **ConPTY の生成と後始末一式**（§4） | 再配布版の優先と kernel32 への退避、overlapped の名前付きパイプ、`STARTF_USESTDHANDLES`、子のハンドルを読みの待ち合わせに入れる終了検知、`ClosePseudoConsole` を別スレッドで閉じて EOF まで読む。どれも理由がコメントに書いてあり、Rust に移すだけでよい |
| 2 | **読みのスレッドで VT の解釈まで行う** | Lisp スレッドは描くだけになる。EmbTerm では「描く」も別スレッドにできる |
| 3 | **端末の返事は待たない**（詰まっていたら捨てる） | ロックを持ったまま書き込みで待つと固まる |
| 4 | **Lisp からの書き込みは `C-g` で抜けられるようにする** | 20 ms ごとの `should_quit` / `process_input` |
| 5 | **イベントのバッファがあふれたらロックを外してから流す** | パイプ ↔ ロックの循環待ちを避ける |
| 6 | **`open_channel` を静的 CRT で使う回避策**（§5.3） | ポーリングをやめて push にできる可能性がある（T-014）。ただし emacs クレートで `open_channel` を呼べるかが別に要る（T-026） |
| 7 | **OSC は解釈済みのものを受け取る**（自前で再走査しない） | C ABI で取れるなら。取れなければ旧 ghostel のスキャナ（`0b4e8cd^:src/module.zig`）が参考になる |
| 8 | 同期出力の扱い（`render_hold` で 1 回通す、resize で 2026 を消さない） | `src/handler.zig:137-140`、`src/Renderer.zig:1053-1062` |
| 9 | 描画のタイミング（キー入力の直後はすぐ、出力が続く間は 30 fps、見えない間は描かない） | §5.2 の表 |
| 10 | mode 2027 を既定で ON、Kitty の媒体（ファイル・共有メモリ）は既定で OFF、貼り付けは 1 回の write、セルのピクセルは 1 以上 | 細かいが、どれもアプリの互換性や安全に効く |
| 11 | `resize_pull_scrollback = false`（ConPTY のとき） | 同じ ConPTY + libghostty-vt なので EmbTerm でも要る（C ABI で設定できるかは T-020） |

### 8.2 真似しないほうがよい

| # | 点 | 理由と代わり |
|---|---|---|
| 1 | **libghostty の内部構造（PageList・Pin・page.dirty）を直接触る** | C ABI からはできないうえ、版が上がると壊れる。EmbTerm は render state の API で描く |
| 2 | **Emacs のバッファに描き、グリフを `min-width` と `height` で押し込む** | EmbTerm が解こうとしている問題そのもの（`★` が 47% の高さになる）。自前で描き、縮めるか逃がすかは T-013 で決める |
| 3 | **Lisp の式をテキストでパイプに流して `eval` する** | 文字列のエスケープは `\` `"` 改行だけ（`src/NativeProcess.zig:246-257`）で、壊れた式は捨てるしかない。EmbTerm は module 側に種類の決まったイベントの列を持ち、Lisp はそれを取り出す形にする |
| 4 | **描画中に端末のロックを持ち続ける** | Emacs バッファの書き換えは遅く、その間 VT の解釈が止まる。EmbTerm は render state に写したらすぐロックを離す |
| 5 | **Job Object を使わない** | 孫が残る。EmbTerm は子を `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` の Job に入れる（推測: conpty.dll が起こす OpenConsole.exe は別扱いなので影響しないはず。T-041 で確かめる） |
| 6 | **子の終了を `INFINITE` で待つ** | バッファを消しても子が終わらなければ読みのスレッドが残る。待ちに時限を付け、過ぎたら Job ごと止める |
| 7 | **キーは押下だけ、文字はエンコーダを通さない、`mapKey` が狭い** | Kitty keyboard を全部満たせない。EmbTerm は（どちらの経路でも）離した・リピート・テンキー・文字も符号化できる形にする |
| 8 | **マウスにセル 1x1 ピクセルを渡す** | SGR-Pixels が正しくない。EmbTerm は実際のセルのピクセルを渡す |
| 9 | `TERM=xterm-ghostty`、`TERM_PROGRAM_VERSION=1.3.2` の固定値 | Windows では terminfo を読むアプリが少なく、版も嘘になる。EmbTerm は `xterm-256color` + `COLORTERM=truecolor` から始め、T-049/T-050 で決める |
| 10 | DA を VT220 として返す、XTVERSION に答えない | 能力を控えめに見せている。EmbTerm で何を名乗るかは T-021 / T-022 の結果で決める |

---

## 9. EmbTerm の決定への含意

| TODO | ghostel から分かったこと | EmbTerm への提案 |
|---|---|---|
| **T-005** VT ライブラリ | ghostel は **C ABI を捨てて Zig API に移った**。1.0 前の C API は OSC 7/52/133 を捨て、スクロールバックの取り出しも回りくどかった | T-020 で **1.0 の C ABI に OSC のコールバック、`write_pty`、render state、`grid_ref`、resize の reflow、`resize_pull_scrollback` 相当、eaw の設定があるか**を最初に確かめる。足りなければ「C ABI + 自前の OSC スキャナ」か「Zig で薄い C ラッパを書いて静的リンク」を比べる |
| **T-006** スレッド | 端末ごとに読み＋解釈のスレッド 1 本。Lisp スレッドが描画。ロックは端末 1 つに 1 本（再帰）。通知は `open_channel` + Lisp の式 | 読み＋解釈のスレッドは同じ形でよい。描画は子 HWND のスレッドで render state を写してすぐロックを離す。返事は待たずに書く。Lisp への通知は「イベントの列 + ポーリング」を基本にし、`open_channel` を CRT の回避策で使うかは T-014 / T-026 で決める |
| **T-007** PTY | `conpty.dll` 優先・kernel32 退避、名前付きパイプ（overlapped）、`STARTF_USESTDHANDLES`、`ClosePseudoConsole` を別スレッド、子のハンドルで終了検知 | `windows` クレートで ghostel と同じ手順を直接書く（`portable-pty` は不要、推測）。**Job Object と時限付きの待ち**を足す。環境変数は unibyte の扱いも含めて elisp 側で組む |
| **T-009** キー入力 | 案 A（Emacs で受けて module で符号化）。semi-char / char / copy の入力モードと例外キーのリスト。押下だけ、文字は素通し | 案 A を採るなら ghostel の入力モードとキーマップの作り（`lisp/ghostel.el:1152-1381`）をほぼ流用できる。ただし離した・リピートを出せないので、Kitty の flags 2 以上を満たすには案 B が要る。IME は案 A なら Emacs 任せで、**変換中の文字列が子 HWND の下に隠れる問題は ghostel には無い**（バッファが画面だから）ので解決策は参考にならない |
| **T-011** 幅を揃える | 幅は libghostty が決め、ghostel は触らない。ConPTY と同じ組み合わせで `★` `※` の桁ずれは起きていない（実測） | libghostty の既定のまま使えば ConPTY と揃う見込み。mode 2027 は conhost が知らないので、絵文字の ZWJ 列で食い違わないか T-025 で測る |
| **T-012** eaw の既定 | libghostty の既定は eaw = 1 桁（実測）。ghostel に切り替えは無い | 既定 1 桁は libghostty の既定と一致。2 桁を選べるようにするには libghostty の設定が要る（無ければ諦めるか、上流に頼む） |
| **T-013** 広いグリフ | `min-width` で桁に固定し、幅・ascent・descent の比の最小で縮める。孤立していれば右隣の空白を借りる | 「縮める」と「孤立時だけ右にはみ出す」の 2 段構えは描画側でもそのまま使える規則。加えて「半角のグリフを持つフォントへ逃がす」を先に試す |

---

## 10. 未確認のこと

- libghostty-vt 1.0 の C ヘッダ（`ghostty/vt.h`）の中身。とくに OSC・XTVERSION・配色の問い合わせのコールバック、
  `resize_pull_scrollback` 相当、eaw の設定、selection / search の C API の有無（T-020）
- `conpty.dll` が `OpenConsole.exe` を探す場所（`x64\` サブディレクトリ）の実際の規則
- 擬似コンソールを閉じたとき、接続中の孫プロセスが本当に終わるか（T-041）
- emacs クレートから `open_channel` を呼べるか（T-026）
