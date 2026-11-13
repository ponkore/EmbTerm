# T-126 段階 2: Emacs module（EmacsView のレイヤーに重ねる）

- 日付: 2026-11-13
- **TODO T-126 の記録**（段階 1 は [t125-macos-render.md](t125-macos-render.md)、検討は [../research/macos-port.md](../research/macos-port.md)）
- 環境: macOS（Darwin 27.2.0）、Emacs 31.1（emacsformacosx の NS 版、aarch64）
- 結論: **Emacs の中で端末が動いた。** `emacs -Q` で端末を開き、キーを送り、日本語と eaw を出し、分割して
  2 つのペインに別々の大きさで置くところまで自動で確かめている（`cargo xtask test-emacs`）

---

## 1. 仕組み（Windows 版との違い）

```text
Lisp（main スレッド）           module                                   描画スレッド（端末ごとに 1 本）
─────────────────────           ──────────────────────                   ────────────────────────────────
embterm-module-place ─────────▶ MacApp（thread_local。レイヤーの置き場）
  …-send-key           ─────▶  Session（et-session。PTY と VT）  ◀────   フレームを写して Metal で描く
  …-poll               ◀─────  Shared（状態と知らせ）           ◀────   描いたら世代を上げる
```

| | Windows（`term.rs`） | macOS（`term_mac.rs`） |
|---|---|---|
| 置くもの | 子 HWND（`WS_CHILD`） | **CAMetalLayer を `EmacsView` のレイヤーの子レイヤーに足す** |
| スレッド | 端末ごとに UI スレッド 1 本（HWND を持つ） | **UI スレッドを作らない**。Lisp からの頼みごとはその場で片づけ、**描画だけ別のスレッド** |
| Lisp → module | 頼みごとを積んで `PostMessage` | その場で実行（AppKit は main スレッドだけ。レイヤーの付け替えは数十 µs） |
| マウス | 子 HWND が受ける（クリック・ドラッグ・ホイール） | **レイヤーは当たり判定に出ない**ので、マウスは Emacs のまま（端末への報告・選択は T-127） |
| フレームの指定 | `window-id` = HWND | `window-id` は NSWindow の番号ではないので、**「z 順の番号 : フレームの幅 : 高さ」**で `EmacsView` を引く |
| フォーカスの報告 | ConPTY が必ず求めるので常に送る（T-070） | **アプリが mode 1004 を求めたときだけ**送る（求めていない `sh` に `^[[O` が文字として出た） |
| キー | KKP / win32-input-mode / レガシー | **KKP / レガシー**（win32-input-mode は ConPTY のためのものなので要らない）。`et_session::keys_mac` |

**なぜレイヤーか**: 子ビュー（NSView）でも重ねられる（T-121 のプローブ）が、ビューはマウスの当たり判定に出るので、
クリックのたびに「どの Emacs のウィンドウを選ぶか」を自分で面倒みることになる。レイヤーなら当たり判定に出ないので、
クリック・ドラッグ・ホイールは**何もしなくても Emacs のまま**動く。実測で `hitTest` が `EmacsView` を返すことを確かめた。

**座標**: `EmacsView` のレイヤー（`EmacsLayer`）は `geometryFlipped = true`（実測）。つまり**左上が原点**で、
`window-inside-pixel-edges`（論理ポイント）をそのまま渡せる。

## 2. 作ったもの

| | |
|---|---|
| `apps/embterm-module/src/term_mac.rs` | macOS の端末 1 つ分（約 1,000 行）。ペイン・描画スレッド・頼みごと・知らせ |
| `apps/embterm-module/src/notify_mac.rs` | module → Lisp の知らせ（T-104 の macOS 版）。Unix では `open_channel` の fd にそのまま `write(2)` でよい（Windows の「Emacs の CRT から `_get_osfhandle` を取る」工夫が要らない） |
| `crates/session/src/keys_mac.rs` | Emacs のキーの名前 → KKP / レガシーの列。`keys_common.rs` に OS 共通の部分（`encode_char` / `parse_mods` / `paste_bytes`）を切り出した |
| `lisp/embterm.el` | module のファイル名を `module-file-suffix` で決める、フレームの指定を OS で分ける（`embterm--frame-key`）、`embterm-shell` の既定を `$SHELL` に、パスの変換を Windows だけに |
| `test/smoke-mac.el` | `emacs -Q` で端末を開いて確かめる自動確認（`cargo xtask test-emacs`） |

## 3. 自動確認（`cargo xtask test-emacs`）

`emacs -Q` を別に起こし、配布物（`target/dist`）の `embterm.el` と `embterm_module.dylib` を読んで確かめる。
**9 項目すべて OK**（11 秒）。

| 確かめたこと | 結果 |
|---|---|
| module が読める（`.dylib`） | 版 0.1.0 |
| `M-x embterm` で端末のバッファができる | `*embterm: sh*` |
| ペインが置かれ、子プロセスが起動する | pid あり、84x24 |
| `embterm-module-send-key` で打った文字がシェルに届く | `hello from emacs` が画面に出た |
| `embterm-module-send-string`（IME の確定文字と同じ経路） | `日本語 ①★※ ─│┌┐` が画面に出た |
| `embterm-module-screenshot` | ペインの画面が PNG になった（Metal で描いたもの） |
| 分割（`split-window-below`） | ペインが 2 つになり、別々の大きさ（84x11 と 84x10）で置かれた |
| `kill-buffer` | 端末が閉じ、Emacs は止まらない |

module のログ:

```text
ペイン 1 を EmacsView のレイヤー（EmacsLayer、595x508、geometryFlipped true）に足した。置き場所 10,2 560x336
起動: pid 31309 / 84x24 / PTY: openpty / フォント HackGen Console NF
ペイン 2 を EmacsView のレイヤー（EmacsLayer、595x508、geometryFlipped true）に足した。置き場所 10,184 560x154
```

## 4. まだ確かめていないこと

- **画面に実際に出ているか（見た目）は未確認。** 画素の読み戻し（module が描いた PNG）と、レイヤーが
  `EmacsLayer` に付いて位置も合っていることは確かめたが、**macOS の画面収録の許可が要るので、
  スクリーンショットでの確認はしていない**。利用者が `emacs -Q -l test/smoke-mac.el` を動かして
  目で見るのが残り（Windows の T-091 と同じ扱い）
- 普段の Emacs の設定（テーマ・corfu・tab-bar など）との組み合わせ

## 5. まだ無いもの（T-127 へ）

| | |
|---|---|
| マウス（クリックでの選択・ドラッグ・ホイール・アプリへの報告） | `embterm.el` の経路は Windows 用。macOS では Emacs 側のイベントから送る形になる |
| copy モード（`C-c C-k`）・リンク（OSC 8）・マウスの下の下線 | `Request::Copy` / `LinkAt` は受けるが何もしない |
| IME（変換中の文字列を端末に描く） | `ns-put-working-text` を奪う（macos-port.md §5-2） |
| 画像（Kitty graphics） | 描画が未対応（T-125 §6） |
| カーソルと文字の点滅、プロンプト間の移動（OSC 133） | タイマーを作っていない |
| クリップボードの読み取り（OSC 52） | `read_system_clipboard` が macOS では `None` |
| Emacs を終えるときの後始末（`embterm-module-shutdown`） | 実装はあるが未確認 |
