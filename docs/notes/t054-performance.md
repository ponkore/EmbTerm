# T-054 性能の測定 / T-068 / T-077 / T-067 / T-096 / T-073 / T-076 / T-097 / T-055

- 日付: 2026-11-11
- **TODO T-054（性能を測る）・T-068（描き直しの負荷と `Present(1)`）・T-096（PNG のデコード）・T-077（部分の描き直し）・T-067（GPU デバイスの共有）・
  T-073（Job から抜けるプロセス）・T-076 後半（閉じ方）・T-097（一瞬の最大化）・T-055（vttest）の記録**。T-006 / T-008 の確定の判断を含む
- 結論:
  - **Emacs は固まらない。** 10 MB / 100 万行の `cat`、画面いっぱいの色付きの文字、4000x4000 の PNG、端末 15 個のどれでも、Emacs の 10 ms タイマーの間隔の p99 は 17〜22 ms、
    最大 21〜31 ms（Windows のタイマーの粒度 15.6 ms の 1〜2 刻み）。`cat` の速さは Windows Terminal と同じくらい（どちらも ConPTY が律速）
  - 変更の前は **UI スレッドが `Present(1)` で 1 フレームごとに 16 ms 止まり**、出力が続く間は頼みごとが平均 23 ms・最大 50 ms 待たされていた。
    **待てるスワップチェーン**にして、`Present` は 0.04 ms、頼みごとの待ちは p99 1.2 ms になった（T-068）
  - 変更の前は**セルごとに色が違う画面（314x70）で 1 フレーム 80 ms** かかっていた。文字を色ごとにまとめ、背景をビットマップ 1 枚で描くようにして **6 ms**（T-008）
  - **GPU デバイスを端末どうしで 1 つにした。** 15 個で Emacs のプライベートメモリ 438 → 123 MB、GPU の共有メモリ 229 → 13.5 MB、スレッド 218 → 64（T-067）
  - **T-006（スレッド）・T-008（描画）は初版の方式で確定してよい。** グリフのアトラス（Direct3D）に移る必要は無い（§4）
  - Job から抜けたのは **Microsoft Store の pwsh（MSIX のパッケージのアプリ）の子**。`PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` で止め、残る経路は閉じるときに親子関係で辿って止める（T-073）
  - 閉じ方は「ClosePseudoConsole（CTRL_CLOSE_EVENT）→ 5 秒まで穏やかに待つ → Job ごと + Job の外の子孫も止める」にした。vim の `.swp` が残るのは
    **書き換えたバッファ**（復旧用に vim が残す）と **MSYS（Git for Windows）の vim**（SIGHUP で常に残す）で、vim の仕様（T-076）

---

## 1. 測る道具

| もの | 中身 |
|---|---|
| `embterm.exe --perf` | 終わるときに UI スレッドの測定（ロックの待ち・持ち、描画、`Present`）と読みのスレッドの測定（ロックの待ち、`vt_write`、バイト数）、PNG のデコードをログに出す |
| `embterm.exe --exit-on-exit` | 子プロセスが終わったらウィンドウを閉じる（`cat` を流して終わる測定用） |
| `embterm-module-perf TERM &optional RESET` | module の測定の報告（文字列。1 行 1 項目、単位 ms）。下の表の項目 |
| `embterm-module-gpu-devices` | プロセスで生きている GPU デバイスの数 |
| `embterm-module-debug-lose-device` | GPU デバイスを失ったことにする（共有のデバイスを捨て、全端末に載せ替えさせる。T-067 の確認用） |
| `embterm-module-process-report TERM` | 子プロセスの木と Job の中身（Job に入っていないものに `!`。T-073 の調べ用） |
| `test/perf.el` | 別の `emacs -Q` で全部を測る（`PERF_TAG=before|after`。結果は `target/t054/perf-TAG.log`） |
| `test/perf-cat.py` | `gen`（10 MB・100 万行のファイルを作る）、`cat`（`cmd /c type` の所要時間を測る）、`colors`（画面いっぱいの色付きの文字。`PERF_COLORS=fg|bg`） |
| `test/kitty-graphics.py big` | 4000x4000 の PNG を Kitty graphics で 1 枚送る（40 桁 x 20 行に縮めて置く） |
| `test/proc-stats.ps1 PID` | ワーキングセット・プライベート・ハンドル・スレッド・CPU 時間・GPU のメモリ（`GPU Process Memory` のカウンタ）・子孫の数 |
| `test/close.el` | 閉じ方・kill-emacs・GPU デバイスの共有と作り直しの自動確認（`target/t054/close.log`） |

module の測定の項目（`embterm-module-perf`）:

| 項目 | 何か |
|---|---|
| `ui.request_delay` | Lisp が頼みごとを積んでから UI スレッドが取り出すまで（**UI スレッドが塞がっている時間**。`perf.el` は出力の最中に 50 ms ごとに何もしない頼みごとを積む） |
| `ui.lock_wait` / `ui.lock_hold` | UI スレッドが端末のロックを待った / 持った時間（render state の更新・画像の写し） |
| `ui.frame_update` | render state から `Frame` へ写した時間（ロックの外） |
| `ui.draw` / `ui.present` | ペイン 1 枚の `BeginDraw`〜`EndDraw` / `Present` |
| `ui.refresh` | 写して描くまでの全体（描いたときだけ） |
| `input.to_ui` / `to_echo` / `to_drawn` / `total` | 入力から表示まで: Lisp が頼んだ → UI スレッドが PTY に書いた → エコーが端末に入った → 描いて `Present` が戻った。`total` が「キーを送ってから画面に描かれるまで」（DWM の合成の 1 フレームは含まない） |
| `read.lock_wait` / `read.vt_write` | 読みのスレッドが端末のロックを待った時間 / `vt_write` の時間（読み 1 回ごと） |
| `png.decode` | PNG のデコード（WIC）の回数・合計・最大。**端末のロックの中**（`vt_write` の中から呼ばれる） |

環境: Windows 11 Pro 26300、Core Ultra 5 228V、Intel Arc 130V、60 Hz、DPI 96、HackGen Console NF 12pt（セル 8x18 px）、再配布版 ConPTY 1.25、release ビルド（`cargo xtask dist`）、
Emacs 31.1（`emacs -Q`、フレーム 120x40 文字 → 端末 119x33）。Windows Terminal はこの PC に入っているもの（版・フォントは確かめていない。既定のプロファイル、`--size 120,36`）。

作り直し方:

```sh
cargo xtask dist
python -I test/perf-cat.py gen target/t054/data
PERF_TAG=after C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/perf.el      # target/t054/perf-after.log
MSYS2_ARG_CONV_EXCL='*' ./target/dist/embterm.exe --cols 120 --rows 36 --exit-on-exit --perf --log target/t054/exe.log \
  -- python -I test/perf-cat.py cat target/t054/data/big10m.txt target/t054/cat.txt exe-big10m
wt.exe -w new --size 120,36 cmd /c "python -I <test\perf-cat.py> cat <data\big10m.txt> <cat.txt> wt-big10m 2> <err.txt>"
C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/close.el                     # target/t054/close.log
```

（Windows Terminal に渡すコマンドは、最後に `2> ファイル` を付けないと `python` が動かなかった。理由は調べていない）

## 2. 数値

「前」は今回の変更の前（測定の道具だけを足したもの。`target/t054/perf-before.log`）、「後」は描画・デバイスの変更の後の測定（17:48）。時間は ms。
閉じ方などを含めた全部の変更の後にもう一度測った（18:20。`target/t054/perf-after.log` はこちら）結果もほぼ同じだった:
`cat` 10 MB 5.5 秒・100 万行 6.9 秒、`ui.request_delay` の p99 は `cat` で 2.4 / 1.1 ms・`colors` で 6.5 ms、`colors` の `ui.draw` は mean 6.1 / p99 7.7、
`input.total` は mean 0.67 / p99 1.36、Emacs のタイマーの最大は 20〜28.6 ms、端末 15 個のプライベート 124 MB・スレッド 64・デバイス 1。

### 2.1 大量の出力（`cmd /c type`）の所要時間

| | 10 MB（10.5 万行、1 行 100 バイト） | 100 万行（13 MB） |
|---|---|---|
| EmbTerm（Emacs の中）前 | 5.1 秒（5.7） | 6.1 秒 |
| EmbTerm（Emacs の中）後 | 5.3〜5.5 秒 | 5.7〜6.9 秒 |
| embterm.exe 前 | 5.6 秒 | 7.3 秒 |
| embterm.exe 後 | 5.0 秒 | 7.0 秒 |
| Windows Terminal | 5.9 秒 | 6.6 秒 |

- **どれもほぼ同じ（約 2 MiB/s）。律速は ConPTY（OpenConsole.exe）**: 読み 1 回は平均 73 バイト（10 MB で 14 万回）、`vt_write` は p99 0.01 ms で、端末側はほとんど何もしていない。
  色付きの文字（`colors`）なら同じ ConPTY でも 18 MiB/s 出るので、`type` の 1 行ごとのスクロールの処理が重い
- 2 回目以降は ±10% ほどぶれる（ほかのプロセス・電源の状態）。前後の差はぶれの中

### 2.2 Emacs の 10 ms タイマーの間隔

| 場面 | 前 p50 / p99 / 最大 | 後 p50 / p99 / 最大 |
|---|---|---|
| 何もしていない | 14.9 / 18.4 / 21.8 | 14.9 / 16.8 / 20.4 |
| `cat` 10 MB | 15.2 / 16.8 / 28.1 | 15.2 / 16.9 / 25.7 |
| `cat` 100 万行 | 15.2 / 16.9 / 25.7 | 15.2 / 16.8 / 26.9 |
| `colors`（最大化 314x70） | 15.3 / 19.1 / 23.0 | 15.4 / 20.0 / 23.2 |
| `cat` 10 MB、上下 2 ペイン | 15.2 / 18.4 / 31.4 | 15.2 / 19.9 / 26.6 |
| 4000x4000 の PNG | 15.2 / 16.6 / 26.6 | 15.2 / 21.9 / 24.6 |
| 端末 15 個（4 個が見える）で何もしていない | 15.2 / 19.8 / 21.7 | 15.2 / 20.7 / 21.9 |
| 端末 15 個で `cat` 10 MB | 15.1 / 26.9 / 29.2 | 15.1 / 20.7 / 27.2 |

`run-at-time` の 10 ms は Windows では 15.6 ms ごとにしか動かないので、p50 の 15 ms が「何も遅れていない」状態。**どの場面でも Emacs は止まっていない**（30 ms を超えたのは前の 2 ペインの 1 回だけ）。

### 2.3 UI スレッドの 1 フレーム（T-068）

| 場面 | | `ui.draw` mean / p99 | `ui.present` mean / p99 | `ui.refresh` p99 | `ui.request_delay` p50 / p99 / 最大 |
|---|---|---|---|---|---|
| `cat` 10 MB（119x33） | 前 | 0.80 / 2.66 | **15.69 / 16.34** | 16.95 | **24.7 / 45.6 / 50.6** |
| | 後 | 1.08 / 2.60 | 0.04 / 0.11 | 2.76 | 0.12 / 1.16 / 1.67 |
| `cat` 100 万行 | 前 | 0.21 / 0.49 | **16.14 / 16.57** | 16.82 | **23.7 / 33.1 / 33.3** |
| | 後 | 0.24 / 0.35 | 0.04 / 0.08 | 0.57 | 0.05 / 0.51 / 0.67 |
| `cat` 10 MB、上下 2 ペイン | 前 | 0.44 / 2.12 | 7.78 / 11.18（2 枚で 1 フレーム） | 16.80 | **24.0 / 33.1 / 33.1** |
| | 後 | 0.58 / 2.09 | 0.04 / 0.09 | 2.21 | 0.06 / 0.84 / 0.96 |
| `colors`（314x70、セルごとに前景・背景の色が違う） | 前 | **78.6 / 93.5** | 0.08 / 0.32 | 94.4 | **65.5 / 164 / 172** |
| | 後 | 6.32 / 7.85 | 0.06 / 0.17 | 8.70 | 0.20 / 5.35 / 7.15 |
| 端末 15 個で `cat` 10 MB | 前 | 0.32 / 2.18 | **16.09 / 16.55** | 16.82 | **23.4 / 33.1 / 46.9** |
| | 後 | 0.44 / 2.33 | 0.05 / 0.11 | 2.42 | 0.06 / 1.12 / 1.75 |

- **前は `Present(1)` が垂直同期を待って 1 回 16 ms 止まり、出力が続く間 UI スレッドはほぼずっと塞がっていた。** Lisp からの頼みごと（キー・置き場所・スクロール）が
  平均 23 ms、最大 50 ms 待たされた（子 HWND の入力キューは Emacs と連結しているので、Emacs のキーの遅れにもつながりうる。GenPreview §1.3 #4）。
  2 ペインでは 1 フレームに `Present` が 2 回で 33 ms
- **後は「次のフレームを受け付けるか」を待たずに確かめ、まだなら描かずにメッセージのループでスワップチェーンの知らせを待つ。** 描く速さは同じ（60 Hz）で、
  `Present` は 0.04 ms、UI スレッドは描くとき以外は空いている
- 描けない間（フレーム待ち・隠れている）は出力の知らせが来ても写さない。そうしないと 1 秒に 1 万回以上ロックを取って写し、CPU が倍になった（最初の版で踏んだ。§3.1）
- 描画（`ui.draw`）は、ふつうの文字の画面で 0.2〜1 ms（p99 2.6 ms）。`cat` 10 MB の 2.6 ms は 1 行 100 文字が画面いっぱいのとき

### 2.4 入力から表示まで

cmd のプロンプトへ 1 文字ずつ（0.15 秒おき、60 回）。Lisp が module の関数を呼んでから、エコーを描いて `Present` が戻るまで。

| | 119x33 | 最大化（314x70） |
|---|---|---|
| 前 `input.total` mean / p99 / 最大 | 0.98 / 1.65 / 1.91 | 1.11 / 1.77 / 1.77 |
| 後 `input.total` mean / p99 / 最大 | 0.70 / 1.50 / 1.74 | 0.91 / 1.49 / 1.49 |
| 後の内訳（mean）: UI スレッドが書くまで / エコー / 描くまで | 0.18 / 0.10 / 0.42 | 0.14 / 0.12 / 0.64 |

**EmbTerm の中の遅れは 1〜2 ms。** 画面に出るのはこのあと DWM が合成する次の垂直同期（最大 16.7 ms）で、これはどの端末でも同じ。Windows Terminal と比べる測定
（画面をカメラ・フォトセンサで見る Typometer のようなもの）はしていない。Emacs のコマンドループ（キーマップを引いて `embterm--self-insert`）の時間は含まない

### 2.5 4000x4000 の PNG（Kitty graphics。T-096）

グラデーションの PNG（542 KB、base64 で 723 KB）を `a=T,f=100` で送り、40 桁 x 20 行に縮めて置いた。

| | 前 | 後 |
|---|---|---|
| `png.decode`（WIC、**端末のロックの中**） | 34.3 ms | 32.7 ms |
| `read.vt_write` の最大（base64 の復号・デコード・画素の保存） | 58.4 ms | 57.5 ms |
| `ui.lock_hold` の最大（`capture_images` が 64 MB の画素を写す） | 21.2 ms | 22.4 ms |
| `read.lock_wait` の最大（読みのスレッドが UI の写しを待つ） | 19.9 ms | — |
| `ui.draw` の最大（BGRA への変換と 64 MB の GPU のビットマップの作成） | 62.3 ms | 60.0 ms（最後の測定では 125 ms） |
| `ui.request_delay` の最大 | 67.5 ms | 64.0 ms |
| Emacs のタイマーの最大 | 26.6 ms | 24.6 ms |
| Emacs のプライベートメモリ（端末 1 個、画像を出したあと） | 257 MB | 222 MB |

embterm.exe（120x36）: デコード 38.3 ms、`vt_write` 最大 65 ms、描画の最大 144 ms。

- **Emacs には響かない**（ロックを取るのは読みのスレッドと UI スレッドで、Lisp は待たない）。UI スレッドは 1 回 60〜80 ms 塞がる（その間のクリック・キーの頼みごとが待つ）
- デコードを端末のロックの外に出すのは **libghostty-vt の API の範囲ではできない**: デコーダ（`GHOSTTY_SYS_OPT_DECODE_PNG`）は `vt_write` の中から同期で呼ばれ、
  画素を返す必要がある。前もって APC を自分で解析してデコードしておく方法は、Kitty graphics の解釈（チャンク・`i` / `I`・圧縮）を二重に持つことになる。**数値の記録で終える**
- 重いのはデコードより、**UI スレッドで 64 MB を BGRA に変えて GPU に載せる**ところ（60〜140 ms）と、ロックの中の画素の写し（20 ms）。
  縮めて置く画像は表示の大きさ（ここでは 320x360 px）まで縮めてから載せれば軽くなる → TODO（§7）

### 2.6 端末を 15 個開いたとき（T-067）

15 個の cmd（2x2 で 4 個が見え、11 個は隠れている）。Emacs のプロセスの値（子の cmd / OpenConsole.exe は含まない）。

| | 端末なし | 1 個 | 15 個（前） | 15 個（後） |
|---|---|---|---|---|
| GPU デバイス | 0 | 1 | **15** | **1** |
| プライベート | 41 MB | 80 MB | **438 MB** | **123 MB** |
| ワーキングセット | 56 MB | 100 MB | 416 MB | 122 MB |
| GPU の共有メモリ（`GPU Process Memory`） | 0 | 13 MB | **229 MB** | **13.5 MB** |
| スレッド | 10 | 24 | **218** | **64** |
| ハンドル | 304 | 390 | 1036 | 665 |
| 何もしていないときの CPU（10 秒） | — | — | 0.47 秒 | 0.41 秒 |
| 子孫（cmd + OpenConsole.exe） | — | 2 | 30 個・394 MB | 30 個・394 MB |

- デバイス 1 つあたり、GPU の共有メモリ 15 MB・スレッド約 10 本（ドライバのスレッド）・プライベート 20 MB ほどだった。共有にして端末 1 個あたりの増え方は
  プライベート 3 MB・スレッド 3 本（UI・PTY の読み・書き）になった
- 子プロセス（cmd + OpenConsole.exe）が 1 組 13 MB ほどで、15 個なら 200 MB 近い。これは ConPTY の仕組み（どの端末でも同じ）
- **何もしていないときの CPU 4〜5%（15 個）は、Lisp の 50 ms ごとのポーリング**（Emacs のメインスレッド）。ポーリングのタイマーを止めると 0 になる
  （`target/t054/idle.log`）。1 回の巡回で 15 個に約 2 ms。module 側（UI スレッド）は何もしていない → TODO（§7）
- `kill-buffer` 15 個は 7 ms（閉じる処理は待たない）

## 3. 判断と変更

### 3.1 T-068: `Present(1)` で UI スレッドが止まる → 待てるスワップチェーンにした

- `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT` + `SetMaximumFrameLatency(1)`。描く前に `WaitForSingleObject(待てるハンドル, 0)` で
  「次のフレームを受け付けるか」を確かめ（`Renderer::try_acquire_frame`）、まだならそのペインを「待ち」にして描かない
- module のメッセージのループを `GetMessageW` から `MsgWaitForMultipleObjectsEx(待ちのペインのハンドル, QS_ALLINPUT, MWMO_INPUTAVAILABLE)` に替え、
  知らされたらそのペインを描く。250 ms 来なければ待たずに描く（保険。今回の測定では起きていない）
- **描けるペインが無い間（どれもフレーム待ち、または全部隠れている）は `WM_APP_OUTPUT` でも写さない。** 端末の「知らせを出した」印（`Session::take_output` を呼ぶまで
  立ったまま）を立てたままにすれば、読みのスレッドはもう知らせてこない。描けるようになったときの `refresh` がまとめて写す。
  これを入れる前の版では、出力の知らせのたびに写して（1 秒に約 1.5 万回）CPU が倍になり、`cat` が 2 割遅くなった。隠れた端末に大量に出しているときも同じで、
  前の版から「隠れた端末の出力でも毎回写していた」のも直った
- embterm.exe も同じ（ハンドル 1 つ）

### 3.2 T-077: 変わった行だけ描くか → **描かない（毎回全体を描く）で確定**

- ふつうの画面の全体の描画は 0.2〜1 ms（p99 2.6 ms）、最悪の色の画面でも後は 6〜8 ms で、1 フレーム（16.7 ms）に十分収まる
- 部分の描き直しには、フリップモデルの 2 枚のバッファの中身を追う（2 フレーム前の差分を描く）か `Present1` の dirty rect の約束（矩形の外は前と同じ中身）を守る必要がある。
  点滅（T-035）のように局所で済む場合だけすでにやっている。それ以上は複雑さに見合わない
- `Present1` の dirty rect（DWM の合成を減らす）も使わない（GPU の負荷は測れる範囲で問題になっていない）

### 3.3 T-008: 描画の方式 → **Direct2D / DirectWrite で確定。グリフのアトラス（Direct3D）には移らない**

前の描き方の弱点は「色・フェイスが変わるたびに `DrawGlyphRun` を 1 回、背景も色の続く範囲ごとに `FillRectangle` を 1 回」で、色の細かい画面で呼び出しが 4 万回を超えること
（314x70 のセルごとに色が違う画面で、文字 70 ms + 背景 10 ms。`PERF_COLORS=fg|bg` で分けて測った）。アトラスに移らずに次の 2 つで解いた:

1. **文字は（フェイス, 大きさ, 色）ごとに画面全体でまとめる。** 位置はグリフのオフセット（`DWRITE_GLYPH_OFFSET`）で渡し、送り幅は 0。
   ただし 1 回の並びを長くしすぎると Direct2D が 1 グリフあたりで重くなる（2 万グリフを 1 回で 170 ms、1024 ずつで 20 ms、128 ずつで 5〜7 ms）ので、
   **128 グリフで区切る**（`MAX_RUN_GLYPHS`）
2. **背景はセル 1 つを 1 画素にしたビットマップ**（既定の背景のセルは透明）を作り、セルの大きさへ最近傍で拡大して 1 回の `DrawBitmap` で描く

結果（embterm.exe 314x70、60 フレーム）:

| | 前 `ui.draw` mean / p99 | 後 mean / p99 |
|---|---|---|
| 前景も背景もセルごとに違う | 65 / 84 | 7.1 / 12.3 |
| 前景だけ | 58 / 76 | — |
| 背景だけ | 10 / 13 | 5.1 / 9.7 |
| ふつうの文字（`cat` 10 MB、314x70） | — | 2.4 / 5.3 |

- 描いた画面は前と**画素まで同じ**（SGR・下線の種類・eaw・絵文字・Nerd Font・選択範囲の画面を前後で撮って比べた。`target/t054/sgr-before.png` / `sgr-after.png`）
- 前は行ごとに「背景 → 文字」だったが、今は範囲の背景をすべて塗ってから文字を描く。下の行へはみ出したグリフが次の行の背景で消えなくなった（WezTerm・Ghostty と同じ順）。
  下線・取り消し線・上線は文字の後
- アトラスに移る理由は、もう測定からは出てこない（最悪の画面で 8 ms、ふつうは 1 ms 前後）。グリフのラスタライズは Direct2D がキャッシュしている

### 3.4 T-006: スレッドの構成 → **初版の構成で確定**

- Lisp のスレッドは、どの場面でも止まらない（§2.2）。Lisp → module はすべて積んで `PostMessage`、module → Lisp はポーリングのまま
- 端末のロックの取り合いはほぼ無い（`read.lock_wait` p99 0.00 ms、`ui.lock_wait` p99 0.13 ms）。ロックを長く持つのは大きな PNG のデコード（§2.5）だけ
- UI スレッドは `Present` で止まらなくなり（§3.1）、描画も最悪 8 ms（§3.3）。UI スレッドが 10 ms を超えて塞がるのは大きな画像を載せるときだけ
- 端末ごとに UI スレッド 1 本（デバイスは共有）。15 個でスレッドは 64 本。1 本の UI スレッドにまとめる必要は無い

### 3.5 T-067: GPU デバイスを端末どうしで共有した

- `Gpu::shared()` がプロセスで 1 つのデバイスを返す（使う人がいなくなったら捨てる。`Weak` で持つ）。世代の番号を持ち、デバイスを失ったら（`EndDraw` / `Present` の失敗）
  `Gpu::invalidate` で捨てて作り直す。ほかの端末は描く前に世代を見比べて載せ替える（`Renderer::follow_shared_device`）。module は失った端末から全端末へ `WM_APP_DEVICE` を投げる
  （同じ世代では 1 回だけ。作り直しに失敗し続けても知らせ合い続けない）
- 1 つのデバイスを複数の UI スレッドで使う: Direct2D のファクトリは `MULTI_THREADED`（Direct2D の呼び出しは Direct2D が排他にする）。Direct2D を通さずに D3D の即時コンテキストに
  触るもの（スワップチェーンの作成・`ResizeBuffers`・`Present`・手放し）は `ID2D1Multithread::Enter/Leave`（`Gpu::lock`）で囲む。即時コンテキストに `SetMultithreadProtected` も掛ける
- **載せ替えでつまずいたこと**: 同じ HWND に前のスワップチェーンが残っていると、新しいデバイスで `CreateSwapChainForHwnd` が `E_ACCESSDENIED` になる。
  前のものを手放し（`SetTarget(None)`・スワップチェーンを落とす）、Direct2D の資源を捨て（`ID2D1Device::ClearResources`）、即時コンテキストを `ClearState` + `Flush` してから作る
  （`Surface::release`）。前のコードのデバイスの作り直しにも同じ問題があった（失うことが無かったので気づかなかった）
- 確かめたこと（`test/close.el`）: 端末 2 個（上下に並べて両方を表示）でデバイスは 1 つ。`embterm-module-debug-lose-device` で失ったことにすると、両方の端末が新しい世代
  （3 → 4）に載せ替え、デバイスは 1 つに戻り、描画の失敗は無く、そのあとの出力も描けた。**本物のデバイスの消失（TDR）は起こしていない**

### 3.6 T-096: PNG のデコードの時間 → 数値の記録で終える

§2.5。デコード 33〜38 ms・`vt_write` 58〜65 ms がロックの中。ロックの外に出す方法は libghostty-vt の API の範囲には無い。Emacs には響かない。

### 3.7 T-073: Job から抜けるプロセス

調べたこと（embterm.exe で、閉じる前に子孫の木と Job の中身をログに出した。`target/t054/job-*.log`）:

| 起こし方 | Job の中か |
|---|---|
| cmd の子（`start /b ping`、`start "" /min ping`、`start "" pythonw`） | 中 |
| embterm.exe が起こした pwsh（Microsoft Store の 7.6.9、MSIX） | 中（作ったあと `AssignProcessToJobObject` で入れる） |
| その pwsh の子（native command、`Start-Process -NoNewWindow`、`Start-Process -WindowStyle Hidden`、`cmd /c start /b`） | **外**（変更の前） |
| cmd の中で `pwsh` と打って起こした pwsh とその子 | **外** |

- 原因は**外側の Job の breakaway ではなく、pwsh が MSIX のパッケージのアプリであること。** パッケージのアプリが起こす子は、既定で「デスクトップアプリの実行環境」の外に作られ
  （`PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE` が既定）、そのとき Job からも外れる。抜けた子は別の Job に入っていた（`IsProcessInJob(..., NULL)` が真）。
  `GetPackageFullName` で pwsh は `Microsoft.PowerShell_7.6.9.0_x64__8wekyb3d8bbwe`
- また、**アプリの実行エイリアス（`%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe`）でパッケージのアプリを起こすと、システムが代わりにプロセスを作る**ので、
  起こした側（cmd）の Job には入らない
- この環境の外側の Job（Claude Code の下）は `BREAKAWAY_OK | SILENT_BREAKAWAY_OK`（0x1800）だったが、cmd の子が Job に入ることから、外側の Job は関係ない
- **変更**: 子を作るときに `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY = PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE` を付ける（子孫に引き継がれる）。
  これで、EmbTerm が直接起こした pwsh の子はすべて Job に入った（native command・`Start-Process` の隠れたもの・新しいコンソールのもの）
- **残る経路**（cmd の中で `pwsh` と打つ、など）は Job に入れられない。**閉じ始めたときに親子関係で子孫を辿り、Job に入っていないものを控えて、猶予の後に止める**
  （`Pty::close`。ハンドルを控えるので PID の使い回しで別のプロセスを止めることは無い）。親が先に終わって木が切れたものは辿れない（残りうる）
- 決めたこと: Job に `BREAKAWAY_OK` / `SILENT_BREAKAWAY_OK` は付けない（子孫を残さない。`code .` のように自分で切り離したいアプリも閉じると止まる。今までどおり）。
  `CREATE_BREAKAWAY_FROM_JOB` で外側の Job から外すこともしない（外側の Job に入ったままでも入れ子の Job で困らない。外側が許さなければ作成が失敗するので、試す分だけ手間）
- 確かめたこと（`test/close.el`）: cmd → pwsh（エイリアス）→ 隠れた ping（Job の外 3 個）と、CTRL_CLOSE_EVENT を受けない pythonw（Job の中）は、閉じてから 5 秒の猶予の後に
  **すべて止まった**。Store の pwsh が起こした ping 2 個（`-NoNewWindow` と隠れたもの）は **Job の中**

### 3.8 T-076: 閉じたときの vim の `.swp` と、閉じ方の順番

閉じ方を次の順にした（`Pty::close(grace)`。すべて `session-close` スレッドで行い、Lisp も UI スレッドも待たない）:

1. Job の外の子孫を控える（§3.7）
2. `ClosePseudoConsole` を別のスレッドで呼ぶ → 中のコンソールのアプリに **CTRL_CLOSE_EVENT**（端末を閉じたときの知らせ。Windows Terminal がタブを閉じるときと同じ。
   MSYS のアプリには SIGHUP になる）。出力を EOF まで読む
3. 子・Job の中・控えた子孫が**すべて終わるのを猶予（5 秒。Windows が CTRL_CLOSE_EVENT の処理を待つのと同じ長さ）まで待つ**
4. 過ぎたら Job ごと止め、控えた子孫も止める

前は「EOF まで 3 秒 → ClosePseudoConsole の戻りを 2 秒 → 子を 2 秒」と内側の時限が積み重なり、外側（`Pty::close(3 秒)`）が先に切れて Job ごと止めることがあった。
また子以外（Job の中のほかのプロセス）が終わるのは待っていなかった。

測ったこと（`閉じた:` のログ。ClosePseudoConsole の戻り / EOF / 全員の終了 / 強制終了の数）:

| 中のもの | 結果 | swap |
|---|---|---|
| Windows 版の vim（scoop 9.x）、書き換えていない | 527 ms で終わる、強制終了 0 | **消える** |
| Windows 版の vim、書き換えた（保存していない） | 26〜529 ms で終わる、強制終了 0 | **残る**（vim が復旧のために残す） |
| MSYS の vim（Git for Windows）、書き換えていない / 書き換えた | 12〜14 ms で終わる、強制終了 0 | **どちらも残る** |
| cmd + CTRL_CLOSE_EVENT を受けない pythonw + Job の外の ping | 5 秒の猶予の後、3 個を強制終了 | — |

- **前に `.swp` が残ったのは MSYS の vim**（cmd の PATH では Git の vim が先に見つかる）で、MSYS の vim は SIGHUP を受けると swap を残して終わる（vim の `preserve_exit`）。
  強制終了ではなかった。Windows 版の vim は書き換えていなければ swap を消して終わる。書き換えたバッファの swap は復旧用で、残るのが正しい
- これ以上「穏やかに」するには、アプリごとの終わらせ方（vim に `:qa`）を送るしかないので、しない

**Emacs を終えるとき（`kill-emacs`）**: 前は `kill-emacs-hook` で閉じる頼みごとを積むだけで、Emacs が先に終わり、Job のハンドルが閉じて中のアプリが**いきなり**止められていた
（CTRL_CLOSE_EVENT が届かない）。Job の外の子孫は残っていた。

- `embterm-module-shutdown GRACE-MS` を足した: 全端末で上の 1〜4 を猶予 GRACE-MS で行い、終わるのを（Lisp のスレッドで）最大 GRACE-MS + 300 ms 待つ
- `embterm.el` の `kill-emacs-hook`（`embterm--close-all`）がそれを呼ぶ。猶予は **`embterm-kill-emacs-timeout`（既定 0.5 秒）**。0 なら待たない（前と同じ）。
  **Emacs を終えるときだけは Lisp を待たせる**（最大 0.8 秒。中のアプリがすぐ終われば、その時間で戻る）
- 確かめたこと: kill-emacs で `kill-emacs-hook` は 518〜538 ms（隠れた ping が CTRL_CLOSE_EVENT を受けないので猶予いっぱい待って止めた）。終わったあと、端末の子孫の PID は
  すべて無い。vim（書き換えた）は 26 ms で穏やかに終わり、swap は復旧用に残った

### 3.9 T-097: embterm.exe が一瞬画面いっぱい（319x75）になる → 再現せず

- 元のログ（`target/t088/yazi-5-q.log`）: 起動 0.1 秒 → 1.68 秒で 319x75 → 3.1 秒で 120x36。319x75 は 2560x1440 のモニタで最大化したときのクライアント領域（セル 8x18）に当たる。
  ログが出るのは描く直前の大きさの確かめ（`sync_size`）なので、時刻は実際の変化より遅れうる
- embterm.exe は最大化しない（`ShowWindow(SW_SHOW)` と `SetWindowPos` でクライアント領域を合わせるだけ。`STARTUPINFO` の表示の指定は `SW_SHOW` の最初の呼び出しで使われうるが、それなら起動の直後に出る）
- 同じような起動を 26 回（ふつう 20 回、yazi 6 回）繰り返したが、一度も起きなかった
- **調べる手がかりを足した**: embterm.exe のログに、大きさが変わるたびの表示の状態（`WINDOWPLACEMENT.showCmd`。3 なら最大化）・ウィンドウの矩形・前面のウィンドウのプロセス、
  `WM_SIZE` の `SIZE_MAXIMIZED`、`WM_SYSCOMMAND`（`SC_MAXIMIZE` / `SC_RESTORE` / `SC_MOVE` / `SC_SIZE`）を出す。次に起きたとき、
  `WM_SYSCOMMAND` があれば利用者の操作（タイトルバーのダブルクリック・Win+↑・スナップ）、無くて `showCmd` が 3 なら別のプロセスの `ShowWindow`、`showCmd` が 1 なら別のプロセスの `SetWindowPos`
  （ウィンドウを管理するツール）と分けられる
- 見立て（推測）: 自動確認の最中に、ほかのウィンドウの操作（利用者の操作、スナップ、ウィンドウを管理する常駐ツール）が前面に出た embterm.exe に当たった

### 3.10 T-055: vttest → 省略

WSL の Ubuntu に vttest が入っていない（`command -v vttest` が空）。apt で入れる操作はしない約束なので、今回は測っていない。

## 4. T-006 / T-008 を確定にできるか

**できる。** requirements.md §6.2 の 3 つ（大量の出力で Emacs が固まらない・入力の遅れが Windows Terminal と同程度・十数個開いても重くならない）は、今回の数値で満たしている:

- 大量の出力: Emacs のタイマーの p99 は 17〜22 ms（粒度 15.6 ms）、`cat` の速さは Windows Terminal と同じ（ConPTY が律速）
- 入力の遅れ: EmbTerm の中で 1〜2 ms（Lisp の呼び出し → `Present` の戻り）。この先は DWM の合成でどの端末も同じ
- 15 個: デバイス 1 つ、Emacs のプライベート 123 MB、スレッド 64 本。何もしていないときの CPU 4〜5% は Lisp のポーリング（§7 の課題）

T-008 のグリフのアトラスは要らない（§3.3）。T-006 は初版のまま（§3.4）。T-068・T-077 も閉じてよい。

## 5. 変えたこと

| 場所 | 変更 |
|---|---|
| `crates/session/src/perf.rs`（新規） | `Samples`（時間の標本、分位点、1 行の報告）。単体テスト 1 件 |
| `crates/session/src/lib.rs` | 読みのスレッドの測定（`perf_report`）。`process_report`（子孫の木と Job）。`CLOSE_TIMEOUT` を 5 秒（穏やかに待つ猶予）に。`close_with(grace)`・`close_report` |
| `crates/vt/src/png.rs` | PNG のデコードの回数・時間（`png_decode_stats`） |
| `crates/render/src/gpu.rs` | **共有のデバイス**（`Gpu::shared`・世代・`invalidate`・`is_current`・`lock`・`live_devices`・`current_generation`）。`SetMultithreadProtected`。単体テスト 1 件 |
| `crates/render/src/surface.rs` | **待てるスワップチェーン**（`try_acquire`・`waitable`・`set_acquired`、`SetMaximumFrameLatency(1)`）。`Present` と `ResizeBuffers` を `Gpu::lock` で囲む。`release`（載せ替えの前に手放す） |
| `crates/render/src/lib.rs` | **背景のビットマップ・文字を色ごとにまとめる描き方**（`draw_backgrounds`・`draw_text`・`MAX_RUN_GLYPHS`）。`DrawStats` に描画と `Present` の時間。`try_acquire_frame` など。デバイスは共有を使い、失ったら全員で作り直す |
| `crates/pty/src/lib.rs` | `PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY`（T-073）。閉じ方（`Pty::close(grace) -> CloseReport`。Job の外の子孫を控えて止める）。`job_pids`・`describe_tree` |
| `apps/embterm-module/src/term.rs` | 測定（`Perf`・入力から表示までの `LatencyProbe`・頼みごとの遅れ）。**メッセージのループ**（`MsgWaitForMultipleObjectsEx`）と、描けないときは写さない。共有のデバイスと `WM_APP_DEVICE`。`shutdown`（Emacs を終えるとき） |
| `apps/embterm-module/src/lib.rs` | `embterm-module-perf`・`-gpu-devices`・`-debug-lose-device`・`-process-report`・`-shutdown` |
| `apps/embterm/src/{args,window}.rs` | `--perf`・`--exit-on-exit`。待てるスワップチェーンとメッセージのループ。閉じる前の子孫の木・閉じ方の記録。T-097 の手がかり（大きさの変化・最大化・`WM_SYSCOMMAND` のログ） |
| `lisp/embterm.el` | `embterm-kill-emacs-timeout`（既定 0.5 秒）。`kill-emacs-hook` で `embterm-module-shutdown` |
| `test/perf.el`・`test/perf-cat.py`・`test/proc-stats.ps1`・`test/close.el`（新規）、`test/kitty-graphics.py`（`big`） | 測定と確認の道具 |

## 6. 確かめたこと

- `cargo build`・`cargo test --workspace`・`cargo clippy --workspace --all-targets` は警告なし。embterm.el の byte-compile も警告なし
- 既存の試験: test/smoke.el・split.el・keys.el・kitty.el・activate.el・integration.el を完走（split・kitty は NG 0、integration は OK 56 / NG 0。
  smoke の応答は何もしていない 19 ms・大量の出力の最中 20 ms、`kill-buffer` 2 つで 0.3 ms）
- `test/close.el`（NG 0。kill-emacs の `kill-emacs-hook` は 517〜538 ms）、`test/perf.el`（前・後）
- embterm.exe: `--perf --exit-on-exit` の `cat`・`colors`、Kitty の `big`、SGR の画面の前後の画素の一致、閉じ方（vim・MSYS の vim・強制終了の経路）

## 7. 未解決・TODO に足すべき課題

1. **何もしていないときの Lisp のポーリングの CPU**: 端末 15 個で 4〜5%（50 ms ごとに全端末で `embterm--place`・`poll`・フォーカス・IME）。隠れている端末は巡回を軽くする
   （ウィンドウに出ていない・ペインが無いなら `embterm--place` を飛ばす）、何も変わらない間は間隔を延ばす、などを試す
2. **大きな画像を載せる UI スレッドの仕事**（64 MB の BGRA 変換と GPU への作成で 60〜140 ms）: 表示の大きさに縮めてから載せる、変換をワーカーでする、
   ロックの中の画素の写し（20 ms）を `Arc` の共有にする、など（T-096 の続き）
3. **T-097 の再現待ち**: 次に起きたら embterm.exe のログの手がかりで原因を分ける
4. **T-055（vttest）**: WSL に vttest を入れてよいか利用者に確かめてから測る（`sudo apt install vttest`）
5. **アプリの実行エイリアス経由のパッケージのアプリ**（cmd の中の `pwsh` など）は Job に入らない。閉じるときに親子関係で辿って止めるが、
   親が先に終わった孫は辿れない。Windows Store の pwsh ではなく MSI 版を使えば起きない（利用者の環境の選択。記録だけ）
6. **`kill-emacs` で最大 0.8 秒待つ**（`embterm-kill-emacs-timeout`）。待たせたくなければ 0 にできるが、そのときはアプリが CTRL_CLOSE_EVENT を受けずに止まる
7. 入力から画面に出るまでの実測（DWM の合成を含む。カメラやフォトセンサ）と Windows Terminal との比較はしていない
8. 本物のデバイスの消失（TDR、ドライバの更新）での作り直しは起こしていない（`dxcap -forcetdr` などで試せる）
9. Windows Terminal に `cmd /c "... "` を渡すとき、最後に `2> ファイル` を付けないと動かなかった理由（測定の手順の問題。EmbTerm とは関係ない）
10. 気づいたこと: SGR の確かめの画面（`target/t054/sgr-*.png`）で、`box: ┌──┬──┐` の `─` が描かれていないように見える（変更の前後で同じ。overlap の既知の見え方 T-013 か、別の問題か未確認）
