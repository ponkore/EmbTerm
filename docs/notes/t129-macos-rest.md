# T-129 macOS の残り: IME の候補の位置・アニメーション・遡った選択・リンクの下線

- 日付: 2026-11-13
- **TODO T-129 の記録**（段階 4 は [t128-macos-perf.md](t128-macos-perf.md)）
- 結論: T-127 / T-128 で置いていったもののうち **4 つを入れた**。残りは「大きな画像の変換を別のスレッドで」（やらない判断）と
  **Windows 機での実ビルドと試験**（機械が要る）

---

## 1. IME の候補ウィンドウの位置（いちばん効くところ）

macOS の入力メソッドは、候補ウィンドウを `-[NSTextInputClient firstRectForCharacterRange:actualRange:]` が
返した矩形の下に出す。Emacs の `EmacsView` はここに**バッファのポイントの位置**を返すので、端末のバッファでは
置き物のテキストの位置（ウィンドウの左上あたり）に候補が出てしまう。

**`EmacsView` のそのメソッドを差し替えた**（method swizzling。`apps/embterm-module/src/ime_mac.rs`）。
Windows の tr-ime の `tr-ime-modadv--set-composition-window`（T-010）に当たる。

- 差し替えは**プロセスで 1 回だけ**。元の実装は覚えておき、**端末を選んでいないとき・別のビューのときは必ず元に渡す**
- 返す矩形は「どのビューの、ビュー座標のどこか」で覚える（描画スレッドが更新する）。
  画面座標への変換は差し替えた実装の中（= main スレッド）で `convertRect:toView:` → `convertRectToScreen:`
- 端末のウィンドウを選んでいない・フォーカスが無い・ペインを消した・閉じたときは忘れて元の動きに戻す

**確かめ方**（`test/smoke-mac.el`）: `embterm-module-ime-rect` で、いま `EmacsView` が IME に返す矩形を読む。

| | 返った矩形（画面座標） |
|---|---|
| 端末を選んでいる（カーソルは行頭） | `(1048, 1070) 7x14` |
| 20 文字打ってカーソルが右へ | `(1178, 1070) 7x14`（**130 pt 右 = 20 桁 x 6.5 pt**） |
| 端末を選んでいない | `(992, 1128) 0x14`（Emacs のポイントの位置。元の動き） |

## 2. Kitty graphics のアニメーション（T-092 の macOS 版）

描く前に `kitty_animation_tick(now_ms)` を呼び、次のフレームの時刻に起こし直す（module の描画スレッドと、
`embterm` のヘッドレス・窓の両方）。**毎回呼ぶ**のが要点で、撮るときだけ 1 回呼んでも進まない（最初それで赤のままだった）。

確かめ方: `python3 -I test/kitty-graphics.py anim`（赤 → 緑 → 青を 300 ms ごと）を、1.2 / 1.5 / 1.8 秒の時点で撮る
→ **赤 → 緑 → 青**と変わる（`target/t129/anim3-*.png`）。

## 3. 遡っているペインでの選択

`et_vt::Terminal::set_selection_screen`（**画面の座標 = スクロールバックを含む行番号**で選択を決める）を足した。
ペインの行 → 画面の行は `total_rows - rows - scroll + 行`。リンクを引くのも同じ計算にした。

確かめ方（`test/features-mac.el`）: `seq 1 200` を出してから 60 行遡り、ドラッグで選択 → `"119"` が取れる。

## 4. マウスの下のハイパーリンクの下線（T-101 の macOS 版）

レイヤーはマウスの動きを受けない（T-126）ので、**Lisp がポーリングのたびに `mouse-pixel-position` から位置を求めて
module に送る**（`embterm--update-hover`）。module は同じ URI が続く範囲を左右に広げて下線の範囲にし、
変わったときだけ `Event::Hover` を出す（URI はエコー領域に出る。Windows と同じ）。

確かめ方: リンクを出して `embterm-module-debug-hover` で位置を送り、`debug-info` の `:hover` に範囲が入ることを見る（`(22 0 4)`）。

## 5. やらないことにしたもの

**大きな画像の変換を別のスレッドで**（T-105 の後半）は**入れない**。

- Windows でこれが要るのは、画像の変換が**UI スレッド**（Emacs と入力キューが連結したスレッド）で走るため
- macOS では**描画スレッド**で走るので、重くても Emacs のキー入力や Lisp は止まらない。
  実測でも 4000x4000 の画像で**その 1 枚だけ 53 ms**（Emacs の 10 ms タイマーには出ない）
- 縮めてから載せる（T-128）ことで常駐も 489 → 422 MB に減っている

## 6. 残り

| | |
|---|---|
| **Windows 機での実ビルドと試験** | この Mac からはクロスの型チェック（`cargo check --target x86_64-pc-windows-gnu`）しかできない。`cargo xtask test-screens` / `test-emacs` を Windows で通すのが残り |
| 普段の Emacs の設定での使い心地（テーマ・corfu・tab-bar・claude・日本語入力の実地） | 利用者の確認待ち |
| copy モードの折り返しの印・選択の自動スクロール（T-084 / T-086 の細部） | 必要になったら |
