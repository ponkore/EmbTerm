# T-063 split / T-037 スクロールバック / T-036 選択・コピー・貼り付け / T-017 copy モード

- 日付: 2026-11-11
- **TODO T-063・T-037・T-036・T-017 の実装の記録**（T-016 の要件、T-038 の一部、T-067 の第一歩を含む）
- 結論: **Emacs のウィンドウごとに子 HWND を 1 枚置くペイン構成（候補 B）にした。** 端末の状態は共有し、ペインごとに別のスクロール位置を持てる。
  端末の大きさは選んでいるペインに合わせ、ほかのペインは切り取り / スクロールバックで埋めて描く。スクロールバック、ドラッグの選択、
  kill-ring・クリップボード連携、貼り付け、スクロールバック丸ごとの copy モードまで、別に起こした `emacs -Q` の自動確認で OK を得た

---

## 1. 作ったもの

| 場所 | 変更 |
|---|---|
| `crates/vt`（et-vt） | `TrackedRef`（`Terminal::track`。行を追いかける位置）、`GridRef` / `Selection` / `Geometry`、`Terminal::gesture_press/drag/release`・`apply_selection`・`has_selection`・`format_plain`（formatter）、`set_scrollback_max_bytes`、`total_rows`・`mouse_tracking`、`Scroll::Row`、`RenderState::set_overscan/overscan`・`all_rows`・`Rows::next_row`、`MouseEncoder`（`MouseAction`・`MouseButton`・`MouseGeometry`）、`paste_is_safe`。FFI と ABI の照合テスト、動作のテスト（追いかける位置・overscan・スクロールバックを含む formatter・ドラッグの選択・マウスの報告）を追加 |
| `crates/render`（et-render） | `FrameView`（`view.rs`）・`Renderer::draw_view`（フレームの一部を、ペインの桁・行で切り取って描く。上の足りない行は空き）・`Renderer::with_gpu` / `adopt_gpu`（GPU デバイスの共有）。`Frame` に `viewport_top`（overscan を入れた行のうちビューポートの先頭）を足した。`Renderer::draw` は今までどおり（embterm.exe はそのまま） |
| `crates/session`（et-session） | スクロールバックのバイト上限を無制限にした（**libghostty の既定は 10,000 バイトで、行数の上限と無関係に数百行しか残らない**）。行数の上限（`scrollback_lines`）だけで決まる |
| `apps/embterm-module/src/term.rs` | 全面的に書き直した（§2）。`App` はペインのリストと共有の `master` フレームを持ち、ペインごとの位置・描画・マウス・選択を扱う |
| `apps/embterm-module/src/lib.rs` | `place` はペイン番号つき、`hide TERM PANE`（nil は全部）、`remove`、`select`、`scroll TERM PANE KIND N`（"lines" "pages" "top" "bottom" "wheel"）、`set-scrollback`、`selection`（ロックなし）、`clear-selection`、`request-copy`、`take-copy`、`paste-safe-p`、`pane-text`（デバッグ）、`screenshot TERM PATH PANE`。`create` に SCROLLBACK。`poll` の `:panes`（`(ID HWND COLS ROWS SCROLLED)` の列）と、イベント `(clicked . PANE)` `(selected . TEXT)` `(copy-ready)` |
| `lisp/embterm.el` | ウィンドウごとのペインの管理、スクロールのキー、ホイールの転送、kill-ring への連携、M-w、copy モードの作り直し、`embterm-scrollback-lines`・`embterm-copy-on-select`・`embterm-confirm-unsafe-paste` |
| `test/split.el`・`test/post-mouse.ps1` | 別の `emacs -Q` で split・スクロール・選択・貼り付け・copy モード・マウスの報告を自動確認する（§4）。`post-mouse.ps1` は子 HWND に `WM_MOUSEWHEEL` / ボタン / 移動を `PostMessage` する |
| `test/smoke.el` | API の変更に追従（`embterm--pane-window`） |

GPU デバイスは端末につき 1 つ（`App::gpu`。T-067 の第一歩。端末どうしの共有は未）。デバイスを失ったら全ペインで作り直す（`App::rebuild_device`）。

## 2. 構成

- `App`（UI スレッド、端末に 1 本）が `panes: Vec<Pane>` を持つ。`Pane` は子 HWND（`View`）・`Position`（`Follow` か `Anchored(TrackedRef)`）・ペインの桁行・遡っているときのフレーム。`active`（端末の大きさを合わせるペイン）と `selected`（選んでいるペイン）
- **`refresh` の流れ（端末のロックの中）**: (1) いちばん下を追うペイン用の `main_rs`（1 つ）を更新（端末より行が多いペインの分だけ overscan で上の行を写す）、(2) 遡っているペインのうち必要なもの（位置が変わった、選択が変わった、見える範囲がアクティブ領域にかかり出力が来た）だけ、ビューポートをそのペインの位置に動かして写す、(3) ビューポートをいちばん下へ戻す。
  ロックを外してから、`main_rs` を 1 枚の `master` フレームに写し（dirty な行だけ）、追っているペインはそれを切り取って描く（**追っているペインが増えても端末側の読みは 1 回。遡っているペインは出力があってもふつう描き直さない**）
- render state は端末の dirty を読むと落としてしまうので、`main_rs` を**先に**更新し、遡っているペインは毎回 `RenderState::new()` で作り直して全部の行を写す（作るのは必要なときだけ）
- **スクロール位置は `TrackedRef`（ペインの先頭の行を追いかける）で持つ**ので、出力が増えても同じ行を指す（下からの行数にしない）。スクロールバックの上限で行が消えると値を失うので、いちばん古い行へ寄せる。代替画面の間は位置を保ったまま「追う」扱いにする
- 端末の大きさ: `active` のペインの桁行に合わせる（同じなら送らない）。行が足りないペインは下側を見せ、行が余るペインは overscan で埋める（上に足りなければ空き）。桁は切り取るか空ける
- カーソル・IME は選んでいるペインだけ（遡っていれば出さない / `embterm-module-cursor` は nil）
- 選択は端末に 1 つ（libghostty の selection。どのペインにも見える）。ドラッグの解釈は libghostty の selection gesture（ダブル・トリプルクリックも）。マウスの報告が有効で Shift なしのときはアプリへ報告（ホイール・ボタン。encoder は libghostty）、Shift ならいつでも選択
- copy モード: `C-c C-k` → module に `Request::Copy` → UI スレッドで formatter（**端末のロックを UI スレッドでだけ持つ**）→ `(copy-ready)` → Lisp が `take-copy` で受け取って入れる。ペインは隠すだけで消さない（戻ると位置は元どおり）
- Lisp: `embterm--panes`（`(window id placed)` の構造体）。`embterm--place` が `get-buffer-window-list` からウィンドウごとに置く / 消す / 隠す。別のフレームは今のフレームだけ（要件 T-016 の「きれいでなくてよい」）

## 3. キーと設定

- `S-<prior>` `S-<next>` `S-<home>` `S-<end>`: 選んでいるペインをスクロール（Emacs に残す）。ホイールはそのペインだけをスクロール（Emacs 側に来たホイールは `embterm-wheel` が module の "wheel" に転送）。入力（文字・キー・貼り付け）で選んでいるペインはいちばん下へ戻り、選択は消える
- ドラッグで選択、`embterm-copy-on-select`（既定 t）なら kill-ring（と `interprogram-cut-function` 経由でクリップボード）へ。`M-w`: 選択があればコピーして消す、無ければ端末へ送る。`C-y` / `C-c C-y` で貼り付け（ブラケットペーストなら包む。そうでなく改行などを含むなら `embterm-confirm-unsafe-paste` で確認）
- `embterm-scrollback-lines`（defcustom、開いている端末にも反映）

## 4. 確かめたこと

`cargo build`・`cargo test --workspace`・`cargo clippy --workspace --all-targets`（警告なし）・embterm.el の byte-compile（警告なし）。embterm.exe（`--input --screenshot --exit-after`）も動く。

**自動確認**: `C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/split.el`（約 1 分）。結果は **`C:\Projects\EmbTerm\target\t063\`**（`split.log`・`module.log`・PNG）。OK 42 件 / NG 0。`test/smoke.el` も通る（`target/t028/`）。PNG はフレーム全体を PrintWindow で取り、Read で目視した。

| PNG | 何をしたか | 見えたもの |
|---|---|---|
| `frame-split-v.png` | `C-x 2` | 両方のウィンドウに端末。行数の違うペイン（15 / 16 行）で下の端に揃う |
| `frame-scrolled.png` | 上のペインに `WM_MOUSEWHEEL` | 上だけ遡る（line362〜）、下は最新のまま |
| `frame-output-while-scrolled.png` | 下へ出力を足す | 上は同じ行のまま、下に新しい行（`split.log` で内容の一致も確認） |
| `frame-selection.png` | ペインのドラッグ（`post-mouse.ps1` の down / move / up） | 選択の色が付き、kill-ring に `ine363\nline364` |
| `frame-copy.png` | 遡ったペインで `C-c C-k` | スクロールバックごと写し、ポイントは line362（遡っていた位置）。もう一方のウィンドウはバッファの先頭 |
| `frame-after-copy.png` | `C-c C-j` | 子 HWND が戻り、スクロール位置は元どおり |
| `frame-split-h.png` / `frame-split-h-right.png` / `frame-split-widths.png` | `C-x 3`、右を選ぶ、左右の幅を変える | 端末は選んだペインの桁に合わせ（`cols` が変わる）、左は遡ったまま切り取りで描ける |
| `frame-split-small-bottom.png` | 上下で高さの違うペイン（下 8 行） | 端末は小さい下に合わせ、上の大きいペインは overscan でスクロールバックが見える（下の端が最新） |
| `frame-other-buffer.png` | 片方のウィンドウを `*scratch*` に | そのウィンドウだけ `*scratch*` が見え、ペインは 1 枚（端末の子 HWND は上のウィンドウのみ） |
| `frame-pruned.png` | スクロールバックの上限を 100 行にし、遡った状態で 800 行出力 | 落ちた行の代わりに、残っているいちばん古い行が見える |
| `frame-one.png` | `C-x 1` | ペインが 1 枚に戻る |

ほかに `split.log` で確かめたこと: 入力で選んだペインは一番下へ（ほかのペインは遡ったまま）、`S-<prior>` / `S-<end>`、Emacs から転送したホイール（`embterm-wheel`）、ダブルクリックで単語 / トリプルで行（kill-ring には最後の 1 項目だけ）、`M-w`（選択をコピーして消す）、クリップボードへ渡す（`gui-select-text`）、貼り付け（cmd は改行なし、pwsh で改行入りの文字列）、pwsh で `?1000h` のときホイールはスクロールせずアプリへ送る（マウスの報告が有効）・Shift+ドラッグは選択になる、スクロールバックの上限で行が消えても壊れない。

**性能**: `emacs -Q` で pwsh に 80,000 行を流し（2 ペインで）: Emacs のタイマー間隔の最大 31 ms（1 回）〜 83 ms（もう 1 回）。copy モード（80,000 行、6.8 MB）: UI スレッドの formatter 30 ms、Emacs のタイマー間隔の最大 164 ms（Lisp への受け渡しと挿入）。

smoke.el: 応答（何もしていない 20 ms / 大量の出力の最中 27 ms）、フレームの付け替え・削除の経路も通った。

## 5. 未解決・気づいたこと

1. **ドラッグがペインの外へ出たときの自動スクロールはない**（selection gesture の autoscroll tick は使っていない。タイマーが要る）
2. **貼り付けの確認は pwsh でも出る**（今回の pwsh + PSReadLine は `?2004h` を要求しない。複数行の貼り付けはブラケットされず、`embterm-confirm-unsafe-paste` の確認の対象。cmd も同じ）。実際に複数行を流すと行ごとに実行される。困るならブラケットペーストの扱いを確かめる
3. copy モードのテキストは `unwrap` しない（端末が折り返した行は別の行のまま。行番号とスクロール位置の対応のため）。`M-w` と kill-ring の選択は unwrap する（ghostty と同じ）
4. copy モードの文字列（数 MB）は Lisp への受け渡しと `insert` が一度に走る（80,000 行で最大 164 ms）。もっと多ければ分割が要る
5. 代替画面ではホイールは何もしない（マウスの報告が有効なときだけ送る。mode 1007 の矢印キーへの変換は T-038 の残り）。クリック・ドラッグのアプリへの報告（Press / Release / Motion）は実装したが、vim / yazi などの実アプリでの確認はしていない（ホイールの報告を pwsh で ?1000h を立てて確かめただけ）
6. ダブル / トリプルクリックの選択は押すごとに `Selected` が来る。Lisp は `double-click-time` 程度の間に続いた、前の内容を含む kill を置き換える（`embterm--on-selected`）
7. `Present(1)`（垂直同期待ち）はペインごとに呼ぶ。ペインが増えたときの UI スレッドの遅れは測っていない（T-068）
8. 選択が変わるとペインのフォーカス（カーソルの塗りつぶし）が変わるので、別のフレームの端末を行き来したときの見た目は確かめていない
9. pane の並べ替え（ウィンドウをスワップ）・タブ（`tab-bar`）では Lisp の `window` が変わるとペインが作り直され、スクロール位置はそこで失われる

## 6. TODO に足すべき課題

- **ドラッグの自動スクロール**（selection gesture の autoscroll。ドラッグがペインの上下に出たとき）
- **T-038 の残り**: vim / yazi で実機確認。代替画面のホイール（mode 1007 / 矢印キーへの変換）。マウスの形（`MOUSE_SHAPE`）
- **ブラケットペーストの確認**: pwsh（PSReadLine）で複数行の貼り付けがブラケットされない理由（ConPTY を通る `?2004h` の扱い）を確かめ、貼り付けの確認の出し方を決める
- **copy モードの改善**: 折り返しのつなぎ（unwrap と行番号の対応の両立）、とても大きいスクロールバックの分割転送
- **T-067 の残り**: 端末どうしの GPU デバイスの共有
- **window を入れ替えたとき（スワップ・tab-bar）のスクロール位置の引き継ぎ**（`window-state` と Lisp のペインの対応）
