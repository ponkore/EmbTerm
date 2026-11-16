# ADR 0007: Lisp → module は PostMessage、module → Lisp はポーリング（+ パイプの知らせ）

- 日付: 2026-11-11（T-006 / T-014）／ 2026-11-12 に T-104 で知らせを追加
- ステータス: 採用（TODO [T-006](../TODO.md) / T-014 / T-054 / T-104。GenPreview ADR 0006 の続き）

## 背景

module は Emacs のプロセスの中で動く。**Emacs の Lisp スレッドを止めない**ことが最優先で、
子 HWND を持つスレッドも詰まらせてはいけない（入力キューが Emacs と連結するので、
詰まると Emacs のキー入力まで止まる）。

一方、端末は PTY の読み・VT の解釈・描画という重い仕事を常に抱えている。

Lisp へ知らせる手段として Emacs 28 以降の `open_channel`（`make-pipe-process` のパイプに
ワーカースレッドから書く）があるが、GenPreview で **CRT の取り違えでプロセスごと落ちる**ことが
分かっていた（ADR 0006）。`open_channel` が返すのは Emacs の CRT のファイル記述子で、
CRT を静的リンクした module の `_get_osfhandle` にはその番号が無い。

## 検討した案

**行き（Lisp → module）**

| 案 | 評価 |
|---|---|
| 関数の中で同期的に処理する | Lisp が止まる。却下 |
| `PostThreadMessage` でスレッドに頼む | モーダルループ（リサイズのドラッグ中など）で取りこぼすおそれ |
| **頼みごとを列に積んで、HWND 宛てに `PostMessage`** | 取りこぼさない。待たない |

**帰り（module → Lisp）**

| 案 | 評価 |
|---|---|
| `open_channel` をそのまま使う | CRT の取り違えで落ちる |
| module だけ CRT を動的リンクにする | Emacs のビルドが使う CRT に合わせ続けることになる（MSYS2 の MINGW64 は msvcrt、UCRT64 は UCRT） |
| **タイマーでポーリングする** | CRT に依存しない。遅れはポーリングの間隔ぶん |
| **Emacs の CRT から `_get_osfhandle` を実行時に引いてパイプに書く** | ghostel が使っていた手。ポーリングの CPU を減らせる |

## 決定

- **行きは「列に積んで HWND 宛ての `PostMessage`」。** module の関数は待たない
  （例外はデバッグ用の `embterm-module-text` / `pane-text` だけ）。
- **帰りは世代付きの共有の状態を `embterm-module-poll` で読む。**
  いつ読むかは、**module が `open_channel` のパイプに 1 バイト書いて知らせる**（T-104）。
  パイプの fd は Emacs のプロセスが読み込んでいる CRT の `_get_osfhandle` を実行時に引いて HANDLE に直す。
  知らせが使えなければ `embterm-poll-interval`（既定 50 ms）のポーリングに落ちる。
  念のための見回りは `embterm-poll-idle-interval`（既定 0.5 秒）。

スレッドは端末 1 つにつき、UI・PTY の読み・PTY の書き・終了待ち（閉じるときだけ）に分ける。
**ロックを持ったまま PTY に書かない**（相手が詰まると固まる）。
**module の別スレッドから Lisp を呼ばない。**

## 帰結

- 測定（T-054）では、Emacs の 10 ms タイマーの p99 がどの場面でも 17〜22 ms で、Lisp は止まらなかった。
  `cat` 10 MB は 5.3 秒で Windows Terminal と同じ（ConPTY が律速）。
- 知らせを入れる前は、端末 15 個で何もしていないときに Lisp のポーリングが CPU の 4〜6% を使っていた。
  知らせにしたあとは 0.3〜1.6%、クリックからウィンドウが選ばれるまでが 12 ms → 1 ms になった（T-104）。
- module は DLL 1 枚のまま（CRT は静的リンク、再頒布 DLL は要らない）。
  Emacs のビルドが MSVCRT でも UCRT でも動く。
- Lisp から見た状態は常に少し古い。クリックの反映やカーソルの位置には遅れがあり、
  IME の位置合わせではこれが見えた（T-137、[ADR 0005](0005-ime.md)）。
- module の境界（`#[defun]`・wndproc・スレッドの本体・`Drop`）は `catch_unwind` で包む。
  panic で Emacs を落とさないため、`panic = "abort"` にはしない。
