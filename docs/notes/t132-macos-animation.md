# T-132 ウィンドウの大きさが変わると端末がぬるっと動く（CALayer の暗黙のアニメーション）

- 日付: 2026-11-14
- **TODO T-132 の記録**（同じ日に出てきた 2 つ: [t133-macos-preedit.md](t133-macos-preedit.md)、
  [t134-macos-cursor-focus.md](t134-macos-cursor-focus.md)）
- 結論: ペインの `CAMetalLayer` は**自分で足した子レイヤー**なので `setFrame:` が
  既定で 0.25 秒のアニメーションになっていた。置き場所を変える間だけ `CATransaction` で
  アクションを切って直した

---

## 1. 見え方と原因

ミニバッファが複数行になる・ウィンドウを分ける・フォントを変えるなど、
`window-size-change-functions` が動くたびに端末がぬるっと動き、反応が鈍い印象になる。

ペインの `CAMetalLayer` は `EmacsView` のレイヤーに `addSublayer:` で足した**子レイヤー**
（`apps/embterm-module/src/term_mac.rs` の `place`）。

- ビューの裏付けのレイヤー（backing layer）なら、AppKit が暗黙のアニメーションを止めてくれる
- **自分で足した子レイヤーは止まらない。** `setFrame:` のたびに `position` / `bounds` の
  アニメーションが入り、0.25 秒かけて動く。`setHidden:` も薄くなるアニメーションになる

Lisp からの頼みごとはその場で片づける作り（T-126）なので、`place` 自体は遅れていない。
**遅れていたのはアニメーションの分だけ**だった。

## 2. 直し方

レイヤーの付け替え・位置・大きさ・表示を変える間だけ、明示のトランザクションでアクションを切る。

```rust
struct NoAnimation;                       // 落ちても Drop で必ず commit する
impl NoAnimation {
    fn begin() -> Self {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        NoAnimation
    }
}
```

`place` / `hide` / `remove` の 3 か所で `let _no_anim = NoAnimation::begin();`。
`objc2-quartz-core` の feature に `CATransaction` を足した。

## 3. 確かめ方

`embterm-module-debug-info` に **`:layer-animations`**（選んでいるペインのレイヤーで今動いている
アニメーションの数 = `-[CALayer animationKeys]` の数）を足した。
ウィンドウの高さを変えた**直後**に読む（暗黙のアニメーションは 0.25 秒で終わるので、後から見ても分からない）。

| | `:layer-animations` |
|---|---|
| 直す前 | **1**（`position`） |
| 直した後 | 0 |

`test/smoke-mac.el` の「置き場所を変えてもレイヤーのアニメーションが入らない」。
**直す前に戻すと落ちることも確かめた**（確認が本当に効いている）。

## 4. 教訓

- macOS で**自分で足した `CALayer` には暗黙のアニメーションが入る**。位置・大きさ・表示を
  プログラムから変えるなら `CATransaction` でアクションを切る
- 時間で消えるもの（0.25 秒のアニメーション）は、**変えた直後に見る**確認でないと捕まえられない
