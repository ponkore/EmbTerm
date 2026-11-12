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

## 2. T-119 プロンプトの境界（OSC 133）

### 2.1 何ができるか

| キー / 設定 | 中身 |
|---|---|
| `C-c C-p` / `C-c C-n`（`embterm-previous-prompt` / `embterm-next-prompt`） | 前 / 後のシェルのプロンプトの行をペインの一番上にする。前置引数で N 個 |
| `C-c C-o`（`embterm-copy-last-output`） | 直前のコマンドの出力を kill-ring（とクリップボード）へ。端末が折り返した行はつなぐ |
| `embterm-show-exit-code`（既定 t） | 直前のコマンドの終了コードが 0 でなければモードラインに ` [exit 3]` |
| `embterm-command-end-functions` | コマンドが終わったら CODE（終了コード。無ければ nil）と BUFFER で呼ぶ（長いコマンドが終わったら知らせる、など） |

### 2.2 仕組み

- libghostty-vt は OSC 133 を解釈し、**行**（プロンプトの最初の行 / 続きの行）と**セル**（出力 / 入力 / プロンプト）に印を付け、
  コマンドの終わり（133;D）をコールバック（`GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT`）で知らせる。C ABI で読める
  （`ghostty_grid_ref_row` + `GHOSTTY_ROW_DATA_SEMANTIC_PROMPT`、`ghostty_grid_ref_cell` + `GHOSTTY_CELL_DATA_SEMANTIC_CONTENT`）
- **前後のプロンプト**: 起点は**ペインの一番上の行**（Ghostty の `jump_to_prompt` と同じ）。最後の 1 画面の中のプロンプトは一番上にできない
  （下の端を越える）ので、上へは画面の外のプロンプトへ移り、下へは最後の画面に入ればいちばん下（追いかける状態）へ戻る。
  最初の版は「下を追っているときは今のプロンプトを起点」にしていたが、目標が最後の画面の中にあると下の端で止まり、押しても動かなかった
- **直前の出力**: いちばん下から 2 つのプロンプトの行を探し、前のプロンプトの後ろで、プロンプトでも入力（折り返した入力・複数行の入力）でもない
  最初の行から、今のプロンプトの前の行まで（`Session::last_command_output`）
- 2 行のプロンプト（starship など）の 2 行目は「続きの行」と印付けされ、プロンプトの始めとは数えない（単体テスト `two_line_prompt_marks`）
- 再配布版の ConPTY（1.25）は OSC 133 をそのまま流す（PSReadLine の行の描き直しがあっても、印は正しい行に付いた）

### 2.3 シェル側（`etc/` の 3 つに足した）

| シェル | A（プロンプトの前）・B（後） | C（コマンドの始め） | D（終わり・終了コード） |
|---|---|---|---|
| pwsh | `prompt` を包む | PSReadLine の Enter のキーハンドラで `AcceptLine` の後 | 次の `prompt` の始め（`$?` / `$LASTEXITCODE`） |
| bash | `PS1` の前後（`\[ \]`） | `PS0`（bash 4.4〜） | `PROMPT_COMMAND` の先頭（`$?` を保つ） |
| nyagos | `nyagos.prompt` を包む（`io.write`） | `nyagos.preexechook`（**フックの中の `io` には `flush` が無いことがある**） | 次のプロンプトの始め（`ERRORLEVEL`） |

- どれも**プロンプトを決めた後で読み込む**（starship などの後）。2 回読み込んでも包み直さない
- pwsh の Enter のキーハンドラは、利用者が自分で Enter を割り当てていれば上書きする（EmbTerm の中だけ）

### 2.4 確かめたこと（`test/prompt.el`）

- pwsh: 出力が 1 画面より長いコマンドを 2 つ（a・b）と短いもの（last-out）を流し、`C-c C-p` で b → a、`C-c C-n` で b → いちばん下。
  直前の出力は `last-out-1\nlast-out-2`（`C-c C-o` で kill-ring へ）。`cmd /c exit 3` で終了コード 3 とモードラインの `[exit 3]`、
  次の成功で消える。出力の無いコマンドの直前の出力は空
- Git Bash: 直前の出力 `b1\nb2`、`false` で終了コード 1
- nyagos: 直前の出力 `n1`
- 利用者の普段の pwsh の初期化（`~/.emacs.d/etc/pwsh-emacs-shell.ps1`。starship の 2 行のプロンプトと OSC 7）に重ねても、直前の出力と終了コードが取れた
  （その場限りの確認）

## 3. 使い始めるには（利用者の作業）

`~/.emacs.d/etc/pwsh-emacs-shell.ps1` の最後（starship の後）に足す:

```powershell
if ($env:TERM_PROGRAM -eq 'EmbTerm') { . 'C:/Apps/embterm/etc/embterm.ps1' }
```

nyagos は `~/.config/nyagos/nyagos.lua` のプロンプトの設定の後に
`if nyagos.env.TERM_PROGRAM == "EmbTerm" then dofile("C:/Apps/embterm/etc/embterm.lua") end`。
