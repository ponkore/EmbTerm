# T-078 / T-081 / T-114 自動確認の道具、T-082 / T-084 / T-085 / T-086

- 日付: 2026-11-12
- **TODO §8 の T-078・T-081・T-114（自動確認を `cargo xtask` から）、T-082（表示しないまま起動）、T-084（ドラッグの自動スクロール）、
  T-085（複数行の貼り付けの確認）、T-086（copy モードの折り返し・分割）の記録**

## 1. 自動確認（T-078 / T-081 / T-114）

### 1.1 `cargo xtask test-screens [--bless] [--no-dist] [名前の一部...]`

- `test/screens/cases.txt` のケースごとに embterm.exe（`target/dist`）を起動し、`--input` で PTY に入力を送り、画面のテキストを
  `test/screens/<名前>.txt`（期待値）と比べる。行末の空白と末尾の空の行は除いて比べる。撮った PNG・テキスト・ログは `target/screens/`
- 4 つずつ並べて動かす。41 ケースで 1 分半
- ケース:
  - **cmd**（3 つ）: `cmd.exe /d /q /k prompt $g`（バナーもマシン名も出ないので決まった画面になる）。日本語・eaw・罫線、60 桁の折り返し、`cls`
  - **vttest**（38 つ。T-114）: WSL の vttest 2.7。T-055 で合格した画面のうち、メニュー 1・2・3・6・8・9・10・11 の 38 画面。
    入力は T-055 のログ（`target/t055/*.log` の「入力を送る」）から取り出した。**期待値は T-055 で見て確かめた画面のテキストと 38 画面すべて一致**
    （新しく撮ったものと T-055 のものを比べた）。2 回続けて回して同じ結果（決まった画面になる）
- 落ちる項目（libghostty-vt が持たない機能）は入れていない。直ったら足す
- 期待値を作り直すのは `--bless`（違いは `git diff test/screens/` で見る）

### 1.2 `cargo xtask test-emacs [--no-dist] [名前...]`

- `test/*.el` を別に起こした `emacs -Q` で順に動かし、**終了コード**と**ログの `[時刻] NG ...` の行・「時間切れ」**で合否を決める
- 既定で動かすもの（合否を自分で決めるもの）: smoke・integration・split・env・resize・font・frames・copy・kitty・close・cleanup。
  名前を指定したときだけ: keys（送ったキーと届いたものの記録を作るだけ）・perf（測定）
- 11 個で 7 分ほど。**試験の Emacs の窓は前に出るので、その間に打ったキーが入ると落ちることがある**（frames で 1 度あった。
  前のコマンドが cmd の入力行に戻っていた = ↑ が届いた。回し直すと通る）
- cleanup は sh。`Command::new("bash")` は Windows の検索順で **System32 の bash.exe（WSL）を先に見つける**ので、
  環境変数 BASH → PATH（System32 と WindowsApps を除く）→ `C:\Program Files\Git\bin\bash.exe` の順に Git の bash を探す
- Emacs は環境変数 EMACS（既定 `C:/Apps/emacs/emacs-31.1/bin/emacs.exe`）

## 2. T-082 表示しないまま起動する

- **要る**と決めた（裏でコマンドを走らせ、後でバッファを出して見る用途。`embterm-run` の DISPLAY を `ignore` にする）
- module に `embterm-module-start TERM COLS ROWS`（まだ起動していなければ、ペインを置く前に起動する）を足した。
  `embterm--create` は、出す関数がバッファを**どのウィンドウにも出さなかったら**これを呼ぶ。大きさは `embterm-background-size`（既定 80x24）
- 後でバッファを出すと、そのウィンドウの大きさに合わせ直す（今までどおり）
- 確かめたこと（`test/env.el`）: 出さずに起動して 80x24・出力がある、出したら 119x15 に合わせた

## 3. T-084 ドラッグの自動スクロール

- 選択のドラッグがペインの上下の外へ出ている間、50 ms ごとにペインを外へ出た向きにスクロールし、端の行までドラッグを伸ばし直す。
  1 回に動かす行数は外へ出た距離で増やす（セルの高さ 2 つごとに 1 行、最大 5 行）。ボタンを離す・キャプチャを失う・ペインに戻ると止める
- libghostty-vt の gesture の autoscroll tick は**端末のビューポート（1 つ）**を動かすが、EmbTerm はペインごとにスクロール位置を持つので使わない。
  ペインのスクロール（`scroll_pane`）と、今までのドラッグ（`gesture_drag`）を組み合わせた
- 確かめたこと（`test/split.el`）: ペインの上 40 px に出したまま 0.8 秒 → ペインが遡り、選択が 10 行より多い

## 4. T-085 複数行の貼り付けの確認

- **今のまま**と決めた（利用者が選んだ）。ブラケットペーストを使わないアプリ（pwsh の PSReadLine、cmd）に改行を含む文字列を貼るときは確認を出す。
  Windows Terminal の既定（`multiLinePasteWarning`）と同じ。止めたい人は `embterm-confirm-unsafe-paste` を nil にする

## 5. T-086 copy モード

- **折り返し**: バッファの行は端末の行のまま（行番号とペインのスクロール位置を対応させるため）にし、**端末が折り返した行の改行に
  `embterm-wrap` を付ける**。端末のバッファの `filter-buffer-substring-function` がその改行を除くので、`M-w` などのコピーではつながる
  （本当の改行は残る）
  - 行が折り返しているかは libghostty-vt の `ghostty_grid_ref_row` + `ghostty_row_get(GHOSTTY_ROW_DATA_WRAP)`（`Terminal::row_wraps`）。
    折り返す行はほぼ端まで埋まっているので、短い行は尋ねない（16,000 行で 5 ms）
- **分割**: テキストは 1 MiB ずつ（行の区切りで）Lisp に渡し、残りはタイマーで受け取る。`embterm-module-take-copy` は
  `(TEXT TOP POINT WRAPS MORE)` を返す。copy モードを抜けたら残りは捨てる
- 確かめたこと（`test/copy.el`）: 1.17 MB・16,010 行が全部入る、受け取る間の Emacs のタイマーの間隔は最大 23 ms、
  折り返した長い行はバッファでは 2 行・コピーでは 1 行
