# T-135 macOS で遡ってもスクロールバックが出ない

- 日付: 2026-11-14
- 発端: 利用者の報告「`M-x zsh` でコマンドを打ってホイールで上へ送っても、**その時点で画面に出ていた行しか出ない**。
  `ls -la` の上のほうに出ていたはずの行が見えない」
- 結論: **macOS の描画スレッドはビューポートの行しか写していなかった。** 遡っているペインは
  フレームの外（負の行）を描こうとして空きになるので、**スクロールバックは 1 行も出ない**。
  ついでに**ホイールの向きが逆**（`wheel-up` が遡らない）だった

---

## 1. 原因（3 つ重なっていた）

### ① 遡っているペインのフレームを写していない（本体）

Windows の `term.rs` は、遡っているペインを `Position::Anchored`（追いかける位置）で持ち、
`refresh` の中で**ビューポートをその位置へ動かして別の render state に写す**（`p.scrolled`）。

macOS の `term_mac.rs` にはこれが無く、`render_main` は毎回

```rust
rs.update(&term);          // = いちばん下のビューポートだけ
frame.update(&mut rs);
```

しかしていない。ペインは `pane_view` で

```rust
top: frame.viewport_top as i32 - r.scroll as i32   // overscan が無いので viewport_top = 0
```

と、**フレームに無い行（負の行）**を指す。`layout::frame_row` はフレームの外を `None` にする
＝その行は描かれないので、遡ると**上から順に空きになっていくだけ**だった。

深く遡る（`scroll >= rows`）と**画面全体が真っ暗**になる。

### ② `pane_text`（`embterm-module-pane-text`）が常にいちばん古い行を返す

```rust
let first = -(scroll as i64);
term.text_rows(first.max(0) as u32, …)    // 遡っていると負 → 0 に潰れる
```

`text_rows` の座標は**画面の座標（スクロールバックの先頭からの行番号）**なので、`0` は
**スクロールバックのいちばん古い行**。遡っていてもいなくても「最初の数十行」を返していた
（実測: 200 行出したあと `bottom.txt` が `sh-3.2$ seq 1 200` から始まっていた）。

`embterm-module-text`（`screen_text`）は端末のビューポートを読むので**ペインの位置とは無関係**。
自動確認でこちらを見ていると①にも②にも気づけない。**ペインの位置を見るのは `pane-text`。**

### ③ ホイールの向きが逆

Lisp（`embterm-wheel`）は `wheel-up` に **+1**（上 = 遡る）を渡す。Windows の `wheel_pane` も
`notches > 0` を上として扱う。ところが macOS の `scroll` は

```rust
ScrollAmount::Wheel(n) => { … -(n as i64) * WHEEL_LINES }
```

と `Lines` と同じ符号の付け方をしていた（`Lines` / `Pages` は「行の番号を動かす向き」なので
遡るのが負）。そのため **`wheel-up` が下へ（= いちばん下で止まったまま）**、`wheel-down` が遡る、
という逆の動きになっていた。アプリがマウスを見ているとき（vim / yazi）に送る矢印も逆
（`wheel-up` で `CSI B`）。

## 2. 直し方

- `capture_scrolled`（`term_mac.rs`）を足す。**端末のロックの中で**ビューポートをペインの位置へ
  動かし、新しい render state に写して、いちばん下へ戻す。ペインが端末より下まで見るときは
  `set_overscan(0, below)` で足りない分を写す
- `render_main` は遡っているペインのフレームを `HashMap<i64, ScrolledFrame>` で持つ。
  写し直すのは**条件が変わったときだけ**（遡っている行数・桁・行・端末の全部の行数が変わった、
  描き直しの指示が来た、出力があってペインの見える範囲がアクティブ領域にかかる）。
  フレームへの写し（`Frame::update`）は**ロックの外**で（読みのスレッドを止めない）
- 描くときは、遡っているペインだけ自分のフレームと `top` を使う（カーソルは出さない）
- `embterm-module-screenshot` も同じ `capture_scrolled` を通す（画面に出ているものと合わせる）
- `pane_text` は画面の座標で `total - rows - scroll` から `rows` 行を読む（`screen_row` と同じ決め方）
- ホイールは **上が正**に直す（`(n as i64) * WHEEL_LINES`、アプリへ送る矢印も `n > 0` で `CSI A`）

## 3. 確かめ方（`test/scrollback-mac.el`、`cargo xtask test-emacs scrollback`）

`/bin/sh` で `clear` → `seq 1 200` を出し、`lines -10` / `top` / `bottom` と
**Emacs の経路のホイール**（`embterm-wheel` に `wheel-up` / `wheel-down` の出来事を渡す）で動かして、
`embterm-module-pane-text` が返す行を見る。

**描けているかは PNG を gzip した大きさで見る。** `png.rs` は**圧縮しない PNG** を書くので
ファイルの大きさは中身によらず同じ（2,258,830 バイト）。gzip すると「文字が並んだ画面」と
「真っ暗な画面」がはっきり分かれる。

| | 直す前 | 直した後 |
|---|---|---|
| 空の画面（`clear` の直後） | 5,057 | 5,047 |
| いちばん下（200 行目まで） | 23,526 | 23,515 |
| **いちばん上まで遡った画面** | **3,413**（空の画面より小さい = 真っ暗） | **15,673** |

自動確認は 15 項目。直す前は 6 件 NG（200 行目が見える / 185 行目が見えない / いちばん上の行が
見えない / PNG が真っ暗 / `wheel-up` で遡らない / `wheel-down` で戻らない）。

## 4. 残っていること

- **遡っている間に出力が来ると、見ている位置が下へずれる。** macOS は位置を「いちばん下から
  何行上か」（`PaneRender.scroll`）で持っているので、行が増えるとその分だけ中身が動く。
  Windows は `TrackedRef`（追いかける位置）で持っているのでずれない。
  選択の座標（`screen_row`）も copy モードの位置も同じ持ち方なので、直すなら揃えて直す
