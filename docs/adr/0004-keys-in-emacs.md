# ADR 0004: フォーカスは Emacs に残し、キーは Emacs のキーマップで受けて送る

- 日付: 2026-11-11
- ステータス: 採用（TODO [T-009](../TODO.md) / T-069。記録は [../notes/t069-input.md](../notes/t069-input.md)）

## 背景

端末の画面は子 HWND が描く（[ADR 0002](0002-child-hwnd-direct-draw.md)）。
ではキー入力を**どちらが受けるか**を決める必要がある。

## 検討した案

| 案 | 評価 |
|---|---|
| **A: フォーカスを Emacs に残し、Emacs のキーマップで受けて module に送る** | IME の入力コンテキストは Emacs のままなので、`C-\`・漢字キー・w32-ime のモードラインが今までどおり動く。Emacs に残すキー（`C-x` など）をキーマップで素直に分けられる。GenPreview も ghostel も A |
| B: 子 HWND がフォーカスを取り、`WM_KEYDOWN` / `WM_CHAR` を自分で処理する | 本物の仮想キー・スキャンコードが取れる（修飾キーの区別が正確）。しかし IME の状態が別の HIMC になり、Emacs に戻すキーの仕組みを自前で作ることになる。重い |

## 決定

**案 A。** 子 HWND は `WS_EX_NOACTIVATE` と `MA_NOACTIVATE` でフォーカスを取らない。

- `embterm-semi-char-map` に、`embterm-keymap-exceptions`（既定 `C-c` `C-x` `C-u` `C-h` `M-x` `M-:` `C-\`）
  **以外**のキーを `embterm--send-event` で束縛する。並びは ghostel の semi-char と同じ。
- キーマップは `emulation-mode-map-alists` の**先頭**に置く（`cua-mode` などに `C-v` を取られないため）。
  あとから読み込まれたパッケージに前を取られることがあるので、端末を開くたびとポーリングのたびに置き直す。
- `C-c C-k` で copy モードに入るとキーを Emacs に返す。`C-c C-j` で戻る。

Emacs のイベントは仮想キーを持たないので、`et-session` の `keys.rs` が
`VkKeyScanW` / `MapVirtualKeyW` で**作り直して** `KeyEvent` に揃える。そこからアプリの求めに合わせて
3 通りのどれかで送る（T-069）。

| 送り方 | いつ |
|---|---|
| KKP（Kitty keyboard protocol） | アプリが `CSI > u` で求めたとき |
| win32-input-mode（`CSI Vk;Sc;Uc;Kd;Cs;Rc _`） | ConPTY が立てているとき |
| レガシーの VT | それ以外 |

## 帰結

- IME は Emacs のものが動く。変換中のキーは `VK_PROCESSKEY` になって Emacs のイベントにならないので、
  **端末のキーマップは変換中のキーを見ない**（変換中の Enter が端末に送られる心配が無い）。
- 確定した文字列は「文字のキーが N 回押された」のと同じ形で届く。まとめて受ける口は無いので、
  **1 文字につき 1 回コマンドループが回り、1 回 PTY に書く**。これが IME の位置合わせの遅れの一因になった（T-137）。
- キーを離したイベント・AltGr・テンキーの区別は Emacs のイベントからは作れない。
  押して離す（`Tap`）として合成している。
- キーボードで打てない文字（IME の確定文字・絵文字）は、仮想キー 0 + UTF-16 の 1 単位ずつで送る。
- 貼り付けなどの長い文字列は win32-input-mode にしない（1 文字 2 列で重い）。素の文字列で送る。
- `Esc` の遅延が無いこと、`C-@`、`Shift+Enter`、PSReadLine のキーは実機で確かめた（T-091）。
