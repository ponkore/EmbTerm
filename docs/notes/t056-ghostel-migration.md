# T-056 / T-057 ghostel からの移行

- 日付: 2026-11-11
- **TODO T-056（ghostel で使っている機能の洗い出し）・T-057（`~/.emacs.d` に `my-embterm.el` を作って並べる）・
  T-109（子の環境変数）・T-110（`embterm-run`）・T-111（`cargo xtask install`）の記録**
- 結論:
  - ghostel で使っていたもの（4 つのコマンド・pwsh の起動・module の置き場）は EmbTerm にすべてある。フォントの調整は要らなくなる
  - 足りなかったのは **Emacs の `setenv` が子に届かないこと**（利用者の設定は `LANG=ja_JP.UTF-8` を `setenv` している）と、
    **コマンド・環境・バッファ名を指定して開く関数**（`my:claude-term` の置き換えに要る）。どちらも足した
  - `~/.emacs.d` の変更は**パッチとして用意した**（書き込みは権限の判定で止まったため。利用者が当てる）

## 1. ghostel で使っているもの（`~/.emacs.d/user-lisp/my-ghostel.el`）

| ghostel | EmbTerm | メモ |
|---|---|---|
| `C-c s s` `ghostel` | `embterm` | 既存の端末に切り替える。無ければ開く |
| `C-c s n` `ghostel-new` | `embterm-new` | |
| `C-c s p` `ghostel-project` | `embterm-project` | project.el。利用者の設定は `project-find-functions` を整えてあるのでそのまま使える |
| `C-c s b` `ghostel-list-buffers` | `embterm-list-buffers` | |
| `ghostel-module-directory` を straight の外へ | `C:/Apps/embterm/`（`cargo xtask install`） | §3 |
| `ghostel-shell` = pwsh + `etc/pwsh-emacs-shell.ps1` | `embterm-shell` に同じものを入れる | 初期化スクリプト（UTF-8 固定・starship・OSC 7）は本物のコンソールでもそのまま効く。OSC 7 で `default-directory` が追う（確認済み） |
| `ghostel-default` の `:family`（罫線の潰れ対策） | 要らない | EmbTerm は fontset を通らず DirectWrite で描く。`my-appearance.el` の Nerd Font の調整（dired 用）はそのまま残す |
| 有効にしていない統合（compile / shell-command / comint / status-indicator） | 無い | 使っていないので困らない |

ghostel の関数をほかのモジュールから呼んでいるところは無い（`my-appearance.el` のコメントだけ）。

`my-pty`（ptyd + eat）は別。`my:claude-term` が使っている（claude を ConPTY で動かし、`CLAUDE_CONFIG_DIR` を
アカウントごとに差し替えた環境とヘッダ行を付ける）。EmbTerm で置き換えるには「コマンド・作業ディレクトリ・環境を
指定して開く」関数が要る → T-110 で足した。置き換えるかの判断は T-059。

## 2. 足したもの

### 2.1 子の環境変数（T-109）

- 前は、子の環境は **emacs.exe のプロセスの環境**に `TERM` などを足したものだった。Emacs の `setenv` は
  `process-environment` を変えるだけで OS の環境は変えないので、**`setenv` が子に届かなかった**（`make-process` や
  `M-x shell` には届く）
- いまは Lisp が `process-environment` と `initial-environment`（起動時）の**差分**（設定と削除）を module に渡し、
  module が emacs.exe の環境にそれを当てる。`TERM` / `COLORTERM` / `TERM_PROGRAM` は最後に端末が決める
- 差分だけにしたのは、`process-environment` に起動時の ANSI の文字列が復号されないまま残る項目があり、
  往復させると化けるため（`~/.emacs.d` の `my-gitd.el` と同じ注意）
- **`setenv` は値を `locale-coding-system` で符号化したユニバイトの文字列で入れる**ので、渡す前に同じ符号化で戻す
  （最初の版は `あ` を含む値で module の UTF-8 の変換が失敗した）
- PTY の `env` は `Vec<(名前, Option<値>)>`（`None` は削除）になった

### 2.2 `embterm-run`（T-110）

```elisp
(embterm-run COMMAND &key DIRECTORY ENVIRONMENT NAME DISPLAY)
```

- COMMAND はプログラムと引数の文字列のリスト。ENVIRONMENT は `process-environment` の形（`copy-sequence` して
  `setenv` で消したものを渡せば、その変数を消して起動する）。NAME を指定するとタイトルで付け替えない。
  DISPLAY は出す関数（既定 `pop-to-buffer-same-window`）。ヘッダ行は返ったバッファで設定すればよい
- `my:claude-term` なら `(embterm-run (list my:claude-executable ...) :directory dir :environment (my:claude--process-environment config) :name "*claude-term*" :display #'pop-to-buffer)`
  の形で置き換えられる見込み（T-059 で試す）

確かめたこと（`test/env.el`、12 項目 NG 0）: `setenv` の `LANG` が `embterm-new` の子に届く、`TERM=dumb` は端末の値に負ける、
起動時の環境にある変数を ENVIRONMENT で消せる、非 ASCII の値（`あ①`）を渡せる、DIRECTORY・NAME・DISPLAY。

### 2.3 `cargo xtask install [DIR]`（T-111）

- 普段使いの Emacs は `C:/Apps/embterm/`（GenPreview の `C:/Apps/genpreview/` と同じ形）を読む。**`target/dist` を読ませない**:
  embterm.el は module 本体を一時ディレクトリに写してから読むが、`conpty.dll` と `OpenConsole.exe` は配布物の場所のものを
  使い続けるのでロックされ、`cargo xtask dist` と試験が上書きに失敗する
- install は release でビルドして写す（`target/dist` は経由しない）。中身が同じものは写さず、使用中で書けないものは
  `<名前>.old-<時刻>` に名前を変えてから写す（使用中のファイルも名前は変えられる）。退避したものは次の install で、
  使われなくなったものから消す。動いている exe を退避して入れ替えられることを確かめた
- 新しい module に入れ替わるのは Emacs の再起動後。ConPTY は次に開く端末から

## 3. `~/.emacs.d` の変更（パッチ）

`~/.emacs.d` への書き込みは Claude Code の権限の判定（自動モード）で止まったので、変更はパッチにした。
`git apply --check` で当てられることを確かめてある。

| ファイル | 変更 |
|---|---|
| `user-lisp/my-embterm.el`（新規） | `my-genpreview.el` と同じ形。`C:/Apps/embterm/embterm.el` があるときだけ autoload を張り、`embterm-shell` を pwsh + 初期化スクリプトにし、`C-c s s/n/p/b` を割り当てる |
| `user-lisp/my-ghostel.el` | 比べる間、ghostel を `C-c S s/n/p/b` に退かせる |
| `init.el` | `(require 'my-embterm)` を `my-ghostel` の次に |
| `README.md` | モジュールの表・キーの表・「EmbTerm の端末」の節 |
| `CLAUDE.md` | `## my-embterm`（`target/dist` に向けない、module の入れ替えは再起動、フォントは fontset を通らない、バッファはプレースホルダ） |

`my-embterm.el` は別の `emacs -Q` で確かめた（利用者の Emacs には読み込んでいない）: `C-c s s` で pwsh が starship の
プロンプト付きで起動、`cd` で `default-directory` が追う、日本語の入力。byte-compile の警告は defcustom の group だけ
（`my-genpreview.el` と同じ）。

## 4. 残り

- T-057: 利用者がパッチを当て、`cargo xtask install` して、普段の Emacs で ghostel と並べて使う
- T-058: ghostel を外す判断（`my-ghostel.el`、`~/.emacs.d/ghostel/`、`.gitignore`、README / CLAUDE.md / docs/pty/ghostel.md）
- T-059: `my:claude-term` を `embterm-run` で置き換えられるか試し、`my-pty` をやめられるか決める
