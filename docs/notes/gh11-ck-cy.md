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
- Windows の `cargo xtask test-emacs split`（貼り付けの確認を `C-y` → `C-c C-y` に差し替えた分）も、
  利用者が 2026-11-19 に回して **OK**

## 5. 端末の側から OSC 52 で出せるか（2026-11-19 の調べもの）

この件は「アプリが切り取ったものを OSC 52 で出さない」のが発端なので、**シェルの側で出す設定が
書けるか**を調べた。対応するかは決めていない（Issue は立てていない）。

### 5.1 zsh: できる【読・実測】

ZLE の `kill-line` を自前のウィジェットで包み、`$CUTBUFFER` を base64 にして OSC 52 で出すだけ。
EmbTerm で実際に動かし、`kill-ring` に入るところまで確かめた。

```zsh
_osc52_kill_line() { zle .kill-line; printf '\033]52;c;%s\a' "$(printf '%s' "$CUTBUFFER" | base64 | tr -d '\n')" }
zle -N kill-line _osc52_kill_line
```

「あいうえおかきくけこ」の「お」と「か」の間で `C-k` → 画面は「あいうえお」、
Emacs 側は `kill-ring: ("かきくけこ")`。

実用にするなら kill 系をまとめて包む。zsh 5.9.2 の emacs のキーマップでは
`^K kill-line` `^U kill-whole-line` `^W backward-kill-word` `^[d kill-word`
`^[w copy-region-as-kill`（`bindkey -M emacs` で確かめた）。

```zsh
_osc52_copy() { printf '\033]52;c;%s\a' "$(printf '%s' "$1" | base64 | tr -d '\n')" }
for w in kill-line backward-kill-line kill-whole-line kill-word backward-kill-word kill-region copy-region-as-kill; do
  eval "_osc52_$w() { zle .$w; zle -f kill; _osc52_copy \"\$CUTBUFFER\" }"
  zle -N $w "_osc52_$w"
done
```

`zle -f kill` は、`C-k` を続けて押したときに `$CUTBUFFER` へ追記される挙動を保つために要る
（[zsh-workers 2015](https://www.zsh.org/mla/workers/2015/msg02627.html) の指摘。**こちらは未確認**。
上の試しは単発の `C-k` だけ）。

macOS だけでよければ `pbcopy` でも足りる。ただし入るのはシステムのクリップボードで、
`kill-ring` には `C-c C-y`（`current-kill` → `interprogram-paste-function`）が拾う形になる。
既製の `zsh-system-clipboard` は pbcopy / tmux / wl-clipboard / Termux で、OSC 52 は持っていない。

### 5.2 受け取る側（EmbTerm）の長さ【実測】

`embterm-allow-clipboard-write` は既定 t なので設定は要らない。長さは
**1,000 / 4,000 / 16,000 / 64,000 / 256,000 文字で欠けなく届いた**（`a` の繰り返しを
OSC 52 で送って `kill-ring` の長さを見た）。

ghostty の `src/terminal/osc.zig` は `MAX_BUF` が 2048 だが、OSC 52 など長くなりうるものは
アロケータで超えられる作り（第三者のミラーで読んだもの。**公式の現行のコードは未確認**）。
実測はこれと矛盾しない。

### 5.3 nyagos: たぶんできるが、書き方が違う

【読】（[doc/07-LuaFunctions_en.md](https://github.com/nyaosorg/nyagos/blob/master/doc/07-LuaFunctions_en.md)）:

- **Lua の関数をキーに割り当てられる** — `nyagos.key.C_K = function(this) ... end`
- 引数 `this` から `this.text`（行全体）、`this.pos`（カーソル位置。バイト単位、行頭が 1）が読め、
  `this:replacefrom(POS,"TEXT")` で置換、`this:call("FUNCNAME")` で組み込みを呼べる
- `nyagos.write("TEXT")` で標準出力に書ける

【読】見つからなかったもの: **キルバッファを読む API がドキュメントに無い**。
`this:call("KILL_LINE")` で切った内容は Lua 側で受け取れそうにない。

【推】回避: `this.text` と `this.pos` からカーソル以降を自分で取り出し、
`this:replacefrom(this.pos, "")` で消して、取り出した文字列を OSC 52 で出せば `C-k` 相当を
自前で書ける。残る未確認は 2 つ — 行編集の最中に `nyagos.write` でエスケープシーケンスを
そのまま端末へ出せるか、base64 を Lua 側でどう作るか。
ConPTY を OSC 52 が素通りすること自体は [../research/conpty.md](../research/conpty.md) で確認済み
（再配布版 1.22 以降）。

### 5.4 claude: 設定では無理【推】

自前の TUI で行編集していて、切り取った内容を OSC 52 で出す設定は見当たらない。
