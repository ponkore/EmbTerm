# #11 端末の中で C-k で切り取った文字列が C-y で貼り付かない

- 日付: 2026-11-19
- **[Issue #11](https://github.com/ponkore/EmbTerm/issues/11) の記録**（利用者が zsh と claude で日本語を打っていて見つけた）
- 結論: **切り取りと貼り付けが別々の保管場所を見ていた。** `C-k` は端末の中のアプリに届いて
  アプリの中のバッファに貯まるのに、`C-y` は端末に届かず Emacs の `kill-ring` から貼っていた。
  `C-y` も端末へ送るようにした（Issue の案 A）

---

## 1. 症状

zsh で「あいうえおかきくけこ」と打ち、「お」と「か」の間にカーソルを置いて `C-k` で
「かきくけこ」を切り取る（画面からは消える）。次の行で `C-y` を押すと、「かきくけこ」ではなく
**その時点で Emacs の `kill-ring` の先頭にあるもの**が貼り付く。claude のプロンプトでも同じ。

`C-k` で切り取ったものは `kill-ring` にもクリップボードにも入っていない。

## 2. 原因

3 つが重なっている。

### 2.1 `C-k` は端末に素通しされる（これは意図どおり）

`lisp/embterm.el` の semi-char のキーマップは、`C-<英字>` のうち `C-i`（TAB）と `C-m`（RET）
だけを除いて `embterm--send-event` に束縛している。`C-k` もそのまま子プロセスに届く。

受け取った zsh の ZLE は `kill-line` を実行し、切り取った文字列を **zsh 自身の `$CUTBUFFER`**
（zsh の killring）に入れる。claude も同じで、自前の行編集が持つ内部バッファに入る。
どちらも**そのプロセスの中**にあり、Emacs からは見えない。

### 2.2 端末の中の切り取りが外に出る道は OSC 52 だけだが、zsh も claude も出さない

EmbTerm はアプリが OSC 52 で書いたテキストを `kill-ring` に入れる（`embterm--on-clipboard`、
`embterm-allow-clipboard-write` は既定 t。T-039）。だが zsh の `kill-line` も claude も
OSC 52 を出さないので、この道は通らない。

### 2.3 `C-y` は端末に届いていなかった（直したのはここ）

直す前は、`C-y` は上の `C-<英字>` の束縛から**わざわざ除いて**（`'(?i ?m ?y)`）、
`embterm-yank` に束縛していた。`embterm-yank` は `(current-kill 0)`、つまり Emacs の
`kill-ring` の先頭を端末へ送る。

つまり `C-k` は端末の中に貯め、`C-y` は Emacs の `kill-ring` から出すので噛み合わない。

なお直す前は、`embterm-keymap-exceptions` に `"C-y"` を足しても何も変わらなかった。
`[remap yank]` が例外の指定と関係なく定義されていて、Emacs の `yank` 経由で結局
`embterm-yank` に行くため。

## 3. 直し方

Issue の案 A（既定を変える）を採った。前身の ghostel は `C-y` を ghostel の yank にしていた
（[../research/ghostel.md](../research/ghostel.md) §semi-char）が、そこだけ既定を変えている。

- `C-<英字>` の除外を `'(?i ?m ?y)` → `'(?i ?m)` にし、`C-y` を端末へ送る
- `C-y` を `embterm-yank` に束縛していた行を消す
- `S-<insert>` と `[remap yank]` は残す。`kill-ring` からの貼り付けは `S-<insert>` と
  `C-c C-y`（`embterm-mode-map`）。`[remap yank]` は macOS の `s-v`、`cua-mode` の `C-v` など、
  `yank` に束縛された**他の**キーのために要る
- 結果として `embterm-keymap-exceptions` に `"C-y"` を足すと、`C-y` の直接の束縛が消えて
  `[remap yank]` を通り、`embterm-yank`（`kill-ring` からの貼り付け）に戻る。
  **元の挙動に戻す逃げ道**になるので、README にそう書いた

`test/split.el` の貼り付けの確認は `C-y` を使っていたので `C-c C-y` に直した。

### 端末ドライバの DSUSP（BSD 系）

`cat` のように行規律が効くプログラムに `C-y` を送ると、**BSD 系の端末では DSUSP
（遅延サスペンド）**として解釈されてプロセスが止まる（macOS の `stty -a` の既定は `dsusp = ^Y`）。
これは本物の端末でも同じで、EmbTerm の問題ではない。zsh や claude は raw モードにするので効かない。
自動確認で `cat -v` に送るときだけ `stty dsusp undef` で外している。

## 4. 確認

- `cargo xtask test-emacs features`（`test/features-mac.el` に足した）
  - `C-y` が `embterm--send-event`、`S-<insert>` と `C-c C-y` が `embterm-yank` に束縛されている
  - `cat -v` に `C-y` を送ると画面に `^Y` が出る（キーマップを通してアプリまで届いた）
  - 続けて `C-c C-y` を送ると `kill-ring` の文字列が貼り付く
- 実機（利用者が 2026-11-19 に macOS で確認、**OK**）: zsh と claude で `C-k` → `C-y` が戻ること、
  `C-c C-y` と `S-<insert>` が `kill-ring` からの貼り付けのままであること、
  `M-w` でコピーして `C-c C-y` で貼れること（T-036）
- **Windows の `cargo xtask test-emacs split` は未確認**（macOS では回せない）。
  貼り付けの確認を `C-y` → `C-c C-y` に差し替えたので、次に Windows で回すときに見る
