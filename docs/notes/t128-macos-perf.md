# T-128 段階 4: 性能の測定と、ghostel との比べ

- 日付: 2026-11-13
- **TODO T-122 / T-128 の記録**（段階 3 は [t127-macos-features.md](t127-macos-features.md)）
- 環境: macOS（Darwin 27.2.0）、Apple M1 Max、Emacs 31.1（emacsformacosx の NS 版）、ディスプレイの拡大率 2
- 道具: `cargo xtask test-emacs perf`（`test/perf-mac.el`）、`cargo xtask test-emacs ghostel`（`test/perf-ghostel.el`）
- 結論
  - **Emacs は固まらない。** 10 MB の出力の最中でも、Emacs の 10 ms タイマーの間隔は p99 21.6 ms（最大 57 ms）
  - **ただし、mac の ghostel も固まらない**（p99 16.9 ms）。**この移植の利点は「Emacs が固まらないこと」ではない**
  - 差が出たのは**速さ**（10 MB の `cat` が **0.10 秒 対 0.21 秒**）と、文字幅・グリフの自由（eaw、全角のグリフ、画像）

---

## 1. EmbTerm の測定（`test/perf-mac.el`）

Emacs の 10 ms タイマーの間隔（= Lisp が止まっていないか）。`emacs -Q` + 配布物の module。

| 場面 | p50 | p90 | p99 | 最大 | 50 ms 超え |
|---|---|---|---|---|---|
| 何もしていないとき | 9.9 | 11.6 | 14.5 | 28.2 ms | 0 回 |
| **10 MB（20 万行）の `cat` の最中** | 10.0 | 11.3 | **21.6** | 57.0 ms | 1 回 |
| 画面いっぱいの色付きの文字（180x120 を 120 行） | 10.0 | 11.1 | 13.5 | 30.8 ms | 0 回 |
| 端末 15 個で待機 | 10.0 | 11.7 | 17.9 | 26.4 ms | 0 回 |

module の中の測定（`embterm-module-perf`）

| 項目 | 値 |
|---|---|
| PTY の読み → `vt_write` | 10.49 MiB を 0.094 秒（**112 MiB/s**）。ロックの待ちは p99 0.00 ms、`vt_write` は p99 0.02 ms |
| 描画 1 枚 | 平均 14.5 ms・最大 18.7 ms。**うち垂直同期の待ちが 14.0 ms** なので、描く仕事そのものは 0.5〜1 ms（T-125 の測定と一致） |
| 入力から出力が届くまで（`input.echo`） | p50 0.34 ms |
| 入力から描き終わるまで（`input.draw`） | p50 1.69 ms |
| 大きな PNG（4000x4000 = 1600 万画素）のデコード | 47.8 ms（libghostty-vt のロックの中。T-096 と同じ性質） |
| メモリ | 端末 1 個 171 MB → 15 個 433 MB（Emacs のプロセス全体）。スレッド 16 → 51 |

**Windows（T-054）との比べ**: Emacs のタイマーは同じくらい（Windows は p99 17〜22 ms）。`cat` は **Windows 5.3 秒 → macOS 0.10 秒**
（Windows は ConPTY が律速。macOS の PTY はその制約が無い）。

## 2. ghostel との比べ（T-122 の答え）

利用者の `~/.emacs.d` に入っている ghostel（lisp は straight のリポジトリ、module は `~/.emacs.d/ghostel/`）を
`emacs -Q` に読み込み、**同じ測り方・同じデータ**で比べた。どちらもシェルは `/bin/sh`、`cat` は `/bin/cat`
（最初はシェルのエイリアスで `cat` が pager に化けていて、比べになっていなかった）。

| | **EmbTerm** | **ghostel** |
|---|---|---|
| 何もしていないとき（10 ms タイマーの p99） | 14.5 ms | 13.0 ms |
| 10 MB の `cat` の最中（p99） | 21.6 ms | 16.9 ms |
| 同（最大） | 57.0 ms | 70.5 ms |
| **10 MB の `cat` が終わるまで** | **0.10 秒** | **0.21 秒** |

- **mac の ghostel も Emacs を固めない。** 「大量の出力で Emacs が固まる」は Windows 側の話で、macOS では当てはまらない
- 速さは EmbTerm が約 2 倍（終わるまでの時間）。ただし測り方は「端末が出力を飲み込んで画面に出るまで」で、
  EmbTerm は VT の状態（描画は別スレッドで追いかける）、ghostel はバッファのテキストになるまで、という違いがある
- **入力から表示までの遅れは比べていない**（ghostel 側に測る口が無い。EmbTerm は p50 1.7 ms）
- **この移植の値打ちは、速さよりも「文字幅とグリフの自由」**（eaw の `①` を 1 桁、全角のグリフを縮めない、
  Kitty graphics の画像、`char-width-table` に縛られない）と、**Windows と同じ設定・キーマップを使えること**（requirements.md §2）

## 3. 機能の回帰テスト（`test/features-mac.el`）

`cargo xtask test-emacs features`。`emacs -Q` + 配布物で **12 項目 OK**。

| 確かめたこと |
|---|
| `setenv` が子プロセスに届く（T-109。日本語の値も） |
| `TERM=xterm-256color` / `TERM_PROGRAM=EmbTerm` |
| フレームを狭めると桁数が減り、`stty size` が子にも伝わる |
| フォントを大きくすると桁数が減る |
| 端末の背景が Emacs の `default` の色に追従する（T-034） |
| スクロールバックを遡る・いちばん下へ戻る（`scrolled` の印） |
| シェル統合: `etc/embterm.bash` を読み込んで `embterm_cmd message …`（OSC 51;E。T-118） |
| OSC 8 のハイパーリンクを画素の位置から引ける |
| `kill-buffer` で後始末 |

## 4. 画像を縮めて載せる（T-105 の macOS 版）

表示が元の半分より小さい画像は、**表示の大きさまで縮めてから GPU に載せる**（箱フィルタで平均）。
4000x4000 の PNG で Emacs のプロセスの常駐が **489 MB → 422 MB**。縮小は描画スレッドで行うので Emacs は止まらないが、
その 1 枚だけ 53 ms かかる（Windows は別のスレッドに出している。T-105 の後半は未移植）。

## 5. 自動確認の全体（macOS）

| | |
|---|---|
| `cargo test --workspace` | 56 OK（et-vt 34・et-pty 6・et-render 8・et-session 3・ほか） |
| `cargo xtask test-screens` | 7 ケース（`sh-*`・`cat-echo`・`kitty-basic`） |
| `cargo xtask test-emacs` | `smoke-mac`（16 項目）・`features`（12 項目） |
| `cargo xtask test-emacs perf` / `ghostel` | 測定（既定では動かさない） |
| `cargo check --workspace --target x86_64-pc-windows-gnu` | Windows 側の型の確認（この Mac から） |

## 6. まだ無いもの

| | |
|---|---|
| IME の候補ウィンドウの位置 | `-[EmacsView firstRectForCharacterRange:]` 次第。ポイントの移動か swizzling |
| 遡っているペインでの選択、マウスの下のリンクの下線 | 位置の対応（`TrackedRef` 相当）と、Lisp からのマウスの動きの通知が要る |
| 大きな画像の変換を別のスレッドで（T-105 の後半）、アニメーション（T-092） | |
| Windows 機での実ビルドと試験 | ずっと未（クロスの型チェックだけ） |
| 普段の Emacs の設定での使い心地 | 利用者の確認待ち |
