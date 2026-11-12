# T-118 / T-119 シェル統合（シェルから Emacs を呼ぶ・プロンプトの境界）

- 日付: 2026-11-12
- **TODO T-118（シェルから Emacs の関数を呼ぶ）・T-119（OSC 133）の記録**

## 1. T-118 シェルから Emacs の関数を呼ぶ（OSC 51;E）

### 1.1 形と経路

- シェルは `ESC ] 51 ; E "名前" "引数" ... ESC \`（または BEL）を出す。vterm と同じ形。引数は `"…"` で包み、`\` と `"` はバックスラッシュで逃がす
  （Emacs の `split-string-and-unquote` で戻る）
- ghostel の OSC 52;e は使わない（libghostty-vt がクリップボードとして消費する）
- libghostty-vt の `GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE` + `UNKNOWN_MAX_BYTES`（8192）で、扱わない OSC を中身ごと受ける
  （`Effects::unknown_osc`）→ session が `51;E` で始まり**切れていない**ものだけ `Event::ShellCommand` → module の `(command . "…")`
  → `embterm--shell-command` が `embterm-shell-commands` の関数を `apply`（`eval` はしない。エラーは `message`）
- 再配布版の ConPTY（1.25）は未知の OSC もそのまま流す

### 1.2 Emacs 側の設定

| 設定 | 既定 | 中身 |
|---|---|---|
| `embterm-allow-shell-commands` | t | nil なら呼ばない |
| `embterm-shell-commands` | `find-file` `find-file-other-window` `view-file`（→ `view-file-other-window`） `dired` `dired-other-window` `message` | (名前 . 関数)。**端末に流れた文字（`cat` したファイルなど）でも呼ばれうる**ので、害の無い関数にとどめる |

- 関数は端末のバッファで呼ぶ（`default-directory` はシェルのカレントディレクトリ。OSC 7 で追っていれば）。カレントバッファは呼んだ後に戻す
  （続く知らせを端末のバッファで扱うため。`find-file` などで変わったウィンドウはそのまま）
- 載っていない名前は呼ばずに「`embterm-shell-commands` に足すと呼べます」と知らせる

### 1.3 シェル側（`etc/`。配布物の `C:\Apps\embterm\etc\` に入る）

| ファイル | 読み込み方 |
|---|---|
| `embterm.ps1`（pwsh 7・Windows PowerShell 5.1） | `if ($env:TERM_PROGRAM -eq 'EmbTerm') { . 'C:/Apps/embterm/etc/embterm.ps1' }` |
| `embterm.bash`（Git Bash・MSYS2・WSL） | `[ "$TERM_PROGRAM" = EmbTerm ] && . /c/Apps/embterm/etc/embterm.bash` |
| `embterm.lua`（nyagos） | `if nyagos.env.TERM_PROGRAM == "EmbTerm" then dofile("C:/Apps/embterm/etc/embterm.lua") end` |

- 関数: `embterm_cmd 名前 引数...`、`embterm_find_file パス...`（`find-file-other-window`）、`embterm_dired [パス]`（`dired-other-window`）。
  短い名前はそれぞれのシェルの alias で（例: `Set-Alias ef embterm_find_file`）
- パスはシェルのカレントディレクトリから絶対パスにして送る（pwsh は `$PWD`。Git Bash は `cygpath -w`、WSL は `wslpath -w`）
- nyagos は `nyagos.write` ではなく `io.write`（加工されない）。nyagos の Lua は 5.1 なので `table.unpack` が無い（最初の版で踏んだ）

### 1.4 確かめたこと（`test/shellcmd.el`）

- pwsh: `embterm_find_file 'あ b.txt'`（カレントディレクトリ `日本語 dir`）が別のウィンドウで開く。`embterm_cmd record 'x"y' 'a\b' 'あ'` がそのまま届く
- 許可していない関数（`delete-file`）は呼ばれない。9000 文字の引数（上限で切れる）は捨てる。`embterm-allow-shell-commands` nil で呼ばれない
- Git Bash: `embterm_dired .` でカレントディレクトリの dired。引数がそのまま届く。**LANG が無いと Git Bash は端末からの日本語の入力を UTF-8 として
  読まない**（試験では `LANG=C.UTF-8` で起動。ふだんは設定で入れる）
- nyagos: `embterm_cmd record nyagos あい`
- WSL の bash は確かめていない（`wslpath -w` の経路）
