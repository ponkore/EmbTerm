# ADR 0003: PTY は ConPTY。再配布版を同梱して優先する

- 日付: 2026-11-11
- ステータス: 採用（TODO [T-007](../TODO.md)。調査は [../research/conpty.md](../research/conpty.md)）

## 背景

Windows で子プロセスを端末として動かす手段は ConPTY しかない。問題は**どの ConPTY を使うか**である。

OS 同梱の ConPTY（Windows 11 26300 の conhost）は、VT を素通しせず**画面を作り直す**旧式で、

- Kitty graphics（APC）と Sixel を**捨てる**（yazi の画像プレビューが出ない）
- OSC 52 / 9 / 777 / 7 と同期出力の**位置をずらす**
- 結合文字・ZWJ の幅が揃わない

Microsoft は ConPTY を NuGet（`Microsoft.Windows.Console.ConPTY`）で別配布しており、
**1.22 以降は VT をその場で素通しする**。ghostel も 1.24 を同梱していた。

## 検討した案

| 案 | 評価 |
|---|---|
| `portable-pty` などのクレートを使う | 余計な抽象が入り、ConPTY のフラグ（素通し・書記素の幅）や `OpenConsole.exe` の差し替えに手が届きにくい |
| **`windows` クレートで ConPTY を直接叩く** | ghostel の手順（overlapped の名前付きパイプ、子のハンドルで終了を知る、`ClosePseudoConsole` は別スレッドで呼んで EOF まで読む）をそのまま移せる |
| OS 同梱の ConPTY だけを使う | 画像が出ず、OSC の位置がずれる。受け入れ基準（yazi の画像プレビュー）を満たせない |
| **再配布版を同梱して優先し、無ければ OS 同梱に落ちる** | 画像と OSC が通る。配布物が 2 ファイル増える |

## 決定

**`windows` クレートで ConPTY を直接叩く。再配布版 1.25（`1.25.260930003`）の `conpty.dll` と
`OpenConsole.exe` を同梱して優先し、見つからなければ OS 同梱の ConPTY を使う。**

取得は `cargo xtask fetch-conpty` が nuget.org から行い、**SHA256 と Authenticode の署名を確かめてから**
`vendor/conpty` に置く。

## 帰結

- yazi の画像プレビューが出る。claude・vim・yazi のような「VT だけで描くアプリ」は、
  アプリと端末の文字幅さえ合っていればずれない。
- conhost の幅が効くのは、コンソール API でカーソルを動かすアプリ（cmd の行編集、PSReadLine、nyagos）と、
  conhost が出す `CUP`・`GetConsoleScreenBufferInfo` を読むアプリに限られる。
  それでも端末・conhost・アプリの 3 者が同じ桁数で数える必要がある（[ADR 0006](0006-eaw-width.md)、T-011）。
- 配布物に `conpty.dll` と `x64/OpenConsole.exe` が入る。**使用中はファイルがロックされる**ので、
  普段使いの Emacs には `target/dist` を直接読ませず、別の場所に写して読ませる（T-120）。
- 子孫は Job Object に入れ、閉じるときは `CTRL_CLOSE_EVENT` → 猶予 → Job ごと止める。
  Emacs が異常終了しても `OpenConsole.exe` が残らない（T-073 / T-076）。
- macOS には ConPTY が無いので、`openpty` + `setsid` + `TIOCSCTTY` に置き換えた。
  呼ぶ側を OS で分けずに済むよう、`PtyOptions` の欄は残してある（T-124）。
