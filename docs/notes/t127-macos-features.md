# T-127 段階 3: IME・マウス・copy モード・画像

- 日付: 2026-11-13
- **TODO T-127 の記録**（段階 2 は [t126-macos-module.md](t126-macos-module.md)）
- 結論: **ふだん使う機能はひと通り動いた。** 日本語入力（変換中の文字列を端末に描く）、マウスでの選択、
  copy モード、Kitty graphics の画像（PNG を含む）、カーソルと文字の点滅、クリップボードの読み取り、
  OS の通知。自動確認は 16 項目（`cargo xtask test-emacs`）と画面の回帰テスト 7 ケース

---

## 1. IME（日本語入力）

NS 版の Emacs は変換中の文字列（`ns-working-text`）を**バッファの overlay で描く**ので、端末のレイヤーの下に
なって見えない。そこで `embterm.el` が `ns-put-working-text` / `ns-unput-working-text` に advice を掛け、
**端末のバッファでは module に渡して、カーソルの位置に自前で描く**（`embterm-module-set-preedit`）。

- 描き方: 選択と同じ背景 + 下線で、カーソルの位置から 1 文字ずつ。全角は 2 セル（送り幅で見分ける）。
  カーソルは文字列の終わりに縦棒で出す
- 確定した文字は、ふだんのキーと同じ経路（`self-insert-command` の remap → `embterm-module-send-key`）で届く
- Windows の tr-ime のような位置の上書き（T-010）は要らない（変換中の文字列は自分で描くため）
- **残り**: 候補ウィンドウの位置。`-[EmacsView firstRectForCharacterRange:]` が返すのは Emacs のポイントの
  位置なので、端末のカーソルとは別のところに出る。寄せるには、ポイントを動かすか method swizzling（T-128）

## 2. マウス

レイヤーはマウスの当たり判定に出ない（T-126）ので、**Emacs が受けたイベントを Lisp から module に渡す**。

- `embterm.el`: `<down-mouse-1>` → `embterm-mouse-drag`。`track-mouse` でドラッグを追い、
  `embterm-module-mouse TERM PANE KIND X Y BUTTON MODS`（X Y は `posn-x-y` = ペインの中の論理ポイント）
- module: アプリがマウスを見ていれば（mode 1000 系）**libghostty-vt のエンコーダで報告**、
  そうでなければ**選択**（down = 始点、drag = 伸ばす、up = 確定 → `Event::Selected` → `embterm-copy-on-select` なら kill-ring）
- **Shift を押している間は、アプリがマウスを見ていても選択にする**（端末の慣習）
- Ctrl+クリックがハイパーリンク（OSC 8）なら `Event::Link` を出して Lisp が開く（T-039 と同じ）
- ホイールは前からの経路（`embterm-wheel` の remap）がそのまま効く
- **残り**: 遡っているペイン（スクロールバックを見ている状態）での選択。マウスの下のリンクの下線（T-101）は、
  レイヤーがマウスの動きを受けないので Lisp から送る形になる

## 3. copy モード・プロンプト

- `C-c C-k`（`embterm-copy-mode`）: Windows と同じく、module がスクロールバックごとのテキストを作り、
  1 MiB ずつ Lisp に渡す（T-086 と同じ形）。折り返した行の印も渡す
- `C-c C-p` / `C-c C-n`（OSC 133 のプロンプト間の移動。T-119）: `find_prompt` でその行をペインの一番上にする

## 4. 画像（Kitty graphics）

- `crates/render/src/mac/images.rs`: 配置（libghostty-vt の render state が解決したもの）を
  ビューの行へ移し、**画像 1 枚につき描画の呼び出しを 1 回**（テクスチャを差し替えるため）。
  層（背景色の下・文字の下・文字の上）の順も Direct2D 版と同じ
- **PNG のデコード**（`crates/vt/src/png_mac.rs`）: `CGImageCreateWithPNGDataProvider`
  （ImageIO の自動判別を通さず PNG に決めて読む）→ ビットマップの文脈に描いて RGBA8 にする
- 踏んだ落とし穴 2 つ
  1. **CoreGraphics のビットマップの文脈はストレートアルファ（`kCGImageAlphaLast`）を受け付けない。**
     `PremultipliedLast` で描いてから自分で戻す（libghostty-vt はストレートアルファを期待する）
  2. 上下の反転は**要らない**。ビットマップの文脈は記憶の先頭の行が画像の上の行になる（最初に反転して上下逆になった）
- 確かめ方: `python3 -I test/kitty-graphics.py basic` を回帰テスト（`kitty-basic`）に入れ、PNG を目で見た。
  PNG（f=100）・RGB（f=24）・RGBA（f=32）の 3 枚が同じ向き・同じ色で出る
- **残り**: 大きな画像を表示の大きさまで縮めてから載せる工夫（T-105 相当）と、別のスレッドでの変換。
  アニメーション（T-092）も未対応

## 5. そのほか

| | |
|---|---|
| 点滅 | カーソル（T-035）と文字（SGR 5。T-112）。描画スレッドが 600 ms ごとに切り替える。見えているものが無ければ止める |
| クリップボードの読み取り | `NSPasteboard`（OSC 52。既定では拒む。T-039） |
| 通知 | macOS は `osascript` の `display notification`（Windows の `w32-notification-notify` に当たる）。`"` と `\` は落としてから渡す |
| シェル統合 | `etc/embterm.bash`（OSC 51;E と OSC 133）はそのまま使える（VT の側で処理するため） |

## 6. 自動確認

- `cargo xtask test-emacs`（`test/smoke-mac.el`）: **16 項目 OK**。T-126 の 9 項目に加えて
  **マウスのドラッグでの選択・変換中の文字列・`ns-put-working-text` の経路・カーソルの点滅・copy モード**
- `cargo xtask test-screens`: **7 ケース OK**（`kitty-basic` を追加）
- `cargo test --workspace`: **53 OK**（PNG のデコードの試験 2 本を追加）
- `cargo check --workspace --target x86_64-pc-windows-gnu`: エラー 0

## 7. まだ無いもの（T-128 へ）

| | |
|---|---|
| IME の候補ウィンドウの位置 | ポイントの移動か swizzling |
| 遡っているペインでの選択、マウスの下のリンクの下線 | 位置の対応（`TrackedRef` 相当）が要る |
| 大きな画像の縮小・別スレッドでの変換・アニメーション | T-105 / T-092 の macOS 版 |
| Windows 機での実ビルドと試験 | ずっと未（クロスの型チェックだけ） |
| 普段の Emacs の設定での確認（テーマ・corfu・tab-bar・claude） | 利用者の確認待ち |
