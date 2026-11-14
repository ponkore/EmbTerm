# T-134 フォーカスが無いときもカーソルが塗りつぶしのまま

- 日付: 2026-11-14
- **TODO T-134 の記録**（同じ日に出てきた 2 つ: [t132-macos-animation.md](t132-macos-animation.md)、
  [t133-macos-preedit.md](t133-macos-preedit.md)）
- 結論: **描画スレッドが `focused` に定数 `true` を渡していた。** レンダラは最初から枠だけで
  描ける作りで、Windows 版は正しく渡していた

---

## 1. 原因

ふつうの端末（ghostel・Ghostty・Terminal.app）はフォーカスが外れると、ブロックのカーソルを
**塗りつぶしから枠だけ**に変えて点滅も止める。

レンダラ（`crates/render/src/mac/mod.rs`）は最初からそれを描ける。

```rust
CursorShape::Block if focused => {}          // 塗りつぶしは背景として塗り済み
CursorShape::Block | CursorShape::BlockHollow => { /* 枠だけ（4 本の線） */ }
```

Windows の `term.rs` は `self.focused && self.selected == Some(p.id)` を渡していたが、
**macOS の描画スレッドは定数 `true`** を渡していた（`draw_view(&frame, &view, true, None)`）。
描画スレッドは `MacApp`（main スレッドのもの）を見られないので、フォーカスを渡す道が無かった。

Lisp 側（`embterm--update-focus`）は T-070 からそのままで、フォーカスの出入りは
module に届いていた（`Request::Focus`）。受けた `MacApp::focused` が描画に使われていなかっただけ。

## 2. 直し方

- `Shared` に `focused`（`AtomicBool`）を足し、`Request::Focus` で書く
- 描画スレッドは **ペインごとに `focused_term && r.selected`** を渡す
  （端末のバッファを**選んでいるウィンドウ**のペインだけ塗りつぶし。Windows と同じ判断）
- **フォーカスが無いときは点滅させない**（枠だけのカーソルを出しっぱなし）。
  `view.show_cursor = view.show_cursor && (!blinking || blink_on || !focused)`
- `embterm-module-screenshot` も同じ判断で撮る（画面に出ているものと合わせる）

「フォーカスがある」は Lisp の判断で、**フレームにフォーカスがあり、選んでいるウィンドウが
この端末のバッファで、semi-char モード**のとき。

**単体の `embterm`（デバッグ用の exe）はそのまま**塗りつぶしで描く。
`cargo xtask test-screens` の PNG が走らせ方（窓が前面かどうか）で変わらないようにするため。

## 3. 確かめ方

- `embterm-module-debug-info` に **`:focused`** を足し、`test/smoke-mac.el` で
  `embterm-module-focus` の nil / t が届くこと・**フォーカスが外れただけで描き直すこと**
  （`:full-draws` が増える。T-133 の原因①と同じ仕組み）を見る
- レンダラの単体テスト「フォーカスが無いカーソルは枠だけ」で、セルの**真ん中が塗られていない**ことと
  **上下左右の枠がカーソルの色**であることを画素で確かめる

自動確認の Emacs には OS のフォーカスが無いので、`embterm--update-focus` が計算する値は nil。
フォーカスを t に戻して確かめる手順は、ポーリングに nil へ戻される前に**同じ手順の中で**読む。
