# T-133 日本語の変換中の文字列（preedit）が画面に出ない

- 日付: 2026-11-14
- **TODO T-133 の記録**（同じ日に出てきた 2 つ: [t132-macos-animation.md](t132-macos-animation.md)、
  [t134-macos-cursor-focus.md](t134-macos-cursor-focus.md)）
- 結論: **原因は 2 つ重なっていた。** ① 描画スレッドが「端末の中身が変わったとき」しか描かない
  ② 変換中の文字列をカーソルが見えるときしか描かない。①は**フォーカス・置き場所・テーマ・
  リンクの下線・スクロール位置**にも効いていた

T-127 で「`ns-put-working-text` に advice を掛けて module に渡し、カーソルの位置に自前で描く」を
入れたのに、実際には画面に出ていなかった。自動確認が**撮るとき専用の描画経路**
（`embterm-module-screenshot`。毎回必ず描く）しか見ていなかったので、すり抜けていた。

---

## 1. 原因① 描画スレッドが描き直さない（主因）

描画スレッド（`render_main`）は `Frame::update` が返す「端末の中身が変わったか」で描くかを決め、
変わっていなければ `continue` していた。変換中の文字列は**端末の中身ではない**
（libghostty-vt の render state には出てこない）ので、`mark_dirty` で起こしても描かずに寝ていた。

- カーソルが点滅していれば、点滅の切り替え（600 ms）のついでに描かれる
  → **最大 600 ms 遅れて、半分の時間だけ出る**
- 点滅していなければ（アプリが `CSI ?12l`、`embterm-cursor-blink` が nil など）**永遠に出ない**

同じ理屈で、**フォーカスの出入り・ペインの置き場所・テーマ・マウスの下のリンクの下線・
スクロール位置**も「中身が変わっていない」側なので、同じだけ遅れていた。

`Shared` に `redraw`（`AtomicBool`）を足し、`mark_redraw()`「中身が変わっていなくても描く」で伝える。

- Lisp からの頼みごと（`MacApp::handle` の中）は**すべて `mark_redraw`**
- **出力の追従（`follow_output`）と点滅・アニメーションの起こし直しは今までどおり `mark_dirty`。**
  こちらは中身が変わる側なので、毎回強制すると垂直同期ごとに無駄に描くことになる
- マウスの移動（ホバー）は、下線の範囲が変わったときだけ `update_hover` が頼む
  （マウスが動いている間ずっと描き直さないため）

## 2. 原因② カーソルが見えないときに描いていなかった

変換中の文字列はカーソルの位置に描くが、その位置を `layout::cursor_cell` で引いていた。
これは**端末のモード（DECTCEM）でカーソルが見えるときだけ**位置を返すので、

- 点滅の「消え」の側では変換中の文字列も消える（600 ms ごとに点滅する）
- **アプリがカーソルを隠していると（`civis`。claude のように自分でカーソルを描く TUI）まったく出ない**

`layout::cursor_cell_any`（`visible` を見ない版）を足し、変換中の文字列はこちらで置く。
`FrameView` に `cursor_row`（このペインがカーソルの行を写しているか。点滅や `civis` では false に
しない）を足して、「遡っているペインには描かない」だけを守る。

同じ理由で、**IME の候補ウィンドウの位置**（T-129 の `cursor_rect`）も `visible` と点滅を
見ないようにした。見ていると、カーソルが消えた瞬間に候補ウィンドウが Emacs のポイントの位置へ戻る。

## 3. 本物の IME から Lisp までの経路（調べた結果）

Emacs 31（NS 版）では次の経路で届く。advice で受け取れることの裏付け:

1. `-[EmacsView setMarkedText:selectedRange:replacementRange:]`（`nsterm.m`）が
   `ns-working-text` を置いて `kbd_buffer_store_buffered_event` でキーの列に積む
2. その事象は `ns-put-working-text` という**キー**になり、**`special-event-map`** で引かれる
   （`global-map` ではない。`(lookup-key special-event-map [ns-put-working-text])` → `ns-put-working-text`）
3. `read_char` が `Fcommand_execute` で呼ぶ → **シンボルの関数セル経由なので advice が効く**

`special-event-map` なので、`emulation-mode-map-alists` の先頭に置いた `embterm-semi-char-map` には
**邪魔されない**（端末のキーの束縛とは別の層）。`test/smoke-mac.el` でこの束縛も確かめている
（Emacs 側の作りが変わったら気づけるように）。

advice が端末に渡すのは **semi-char モードの間だけ**にした（`embterm--ns-preedit-here-p`）。
copy モードでは端末のレイヤーを隠して Emacs のバッファを直に見ているので、
そこで isearch などに日本語を打ったときは Emacs 自身に描かせないと見えない。
フォーカスが外れたときは `embterm--update-focus` が変換中の文字列を消す
（ふつうは IME が `ns-unput-working-text` を送るが、送らずに終わる場合の備え）。

## 4. 確かめ方

`embterm-module-debug-info` に **`:preedit`**（いま端末が持っている変換中の文字列）を足した。

`test/smoke-mac.el`:

- `embterm-module-set-preedit` → `:preedit` が入る + **`:full-draws` が増える**
  （= 端末の中身が変わっていないのに描いた。原因①の確認）
- `ns-working-text` を置いて `ns-put-working-text` → `:preedit` が入る（**advice の経路**）
- `ns-unput-working-text` → `:preedit` が消える
- `special-event-map` の束縛があること

描画の回数を数える手順は「**変換中の文字列しか変わらない**手順」に分けた。
前の手順（選択の消去）の描き直しは別のスレッドで遅れて届くので、同じ手順の中で数えると
**強制しなくても数が増えて通ってしまう**（実際に一度それで通した）。
`set_preedit` だけ `mark_dirty` に戻すと落ちることを確かめてある。

レンダラの単体テスト（`cargo test -p et-render`）
「変換中の文字列はカーソルが隠れていても描く」: `visible: false` のフレームで、
カーソルの桁から選択と同じ背景が出て、カーソルより前の桁には出ない（原因②の確認）。

**実機の IME で打ったときの見え方は利用者が確認**（2026-11-14 に OK）。自動確認では本物の
入力メソッドを動かせない（入力ソースの切り替えとキーの合成に許可が要る）。
うまくいかないときは `embterm-module-set-log` を入れておくと、
`IME の変換中の文字列: Some("にほん")` のような行がログに出る
（届いていないのか、描けていないのかが分かる）。

## 5. 教訓

- **撮るとき専用の経路しか見ていない確認は、画面に出ないことを見つけられない。**
  `embterm-module-screenshot` は毎回必ず描くので、「描画スレッドが描き直さない」という
  いちばん多い失敗を通してしまう。`:full-draws` が増えるかを一緒に見る
- 描画の回数で確かめるときは、**数える手順を 1 つのことだけ変わる手順に分ける**。
  別のスレッドの描き直しが遅れて届くと、直っていなくても数が増える
- 端末の見た目のうち「端末の中身ではないもの」（カーソルのフォーカス・変換中の文字列・
  リンクの下線・ペインの置き場所・テーマ）は、**描き直しを別に頼む**必要がある
