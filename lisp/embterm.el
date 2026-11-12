;;; embterm.el --- Emacs のウィンドウに重ねて表示する端末 -*- lexical-binding: t; -*-

;; Version: 0.1.0
;; Package-Requires: ((emacs "31.1"))
;; Keywords: terminals

;;; Commentary:

;; EmbTerm は Windows の Emacs の中で動く端末（docs/requirements.md）。
;; Emacs dynamic module（embterm_module.dll）が、Emacs のウィンドウの本文領域に子 HWND を重ね、
;; そこに端末の画面を描く（ConPTY + libghostty-vt + Direct2D）。このファイルはその Lisp 側（T-029）。
;;
;; - `M-x embterm' で新しい端末のバッファを開く。バッファの中身は置き物（端末は子 HWND が描く）
;; - 位置合わせ: `window-size-change-functions' / `window-buffer-change-functions' /
;;   `window-selection-change-functions' で、端末のバッファを出している**すべてのウィンドウ**に
;;   ペイン（子 HWND 1 枚）を置き（`C-x 2' / `C-x 3'。T-063）、`window-inside-pixel-edges' を module に送る。
;;   別のフレームに同じ端末を出したときは、今のフレームのウィンドウだけに置く（要件 T-016）。ペインごとに
;;   別のスクロール位置を持ち、端末の大きさは選んでいるペインに合わせる（module）
;; - キー（T-009 の案 A）: フォーカスは Emacs に残る。semi-char モードでは
;;   `embterm-keymap-exceptions' 以外のキーを module に送る（ghostel の semi-char と同じ）。
;;   キーマップは `emulation-mode-map-alists' の先頭に置く（cua-mode などに負けないため）
;;   `C-c C-k' で copy モード（キーを Emacs に返し、スクロールバックごとバッファに写す。T-017）、`C-c C-j' で戻る
;; - スクロール（T-037）: ホイールと `S-<prior>' `S-<next>' `S-<home>' `S-<end>' でペインごとに遡る。
;;   入力すると、そのペインはいちばん下へ戻る
;; - 選択（T-036）: 子 HWND のドラッグ（Shift+ドラッグはマウスの報告が有効なアプリの間でも選択）。
;;   `embterm-copy-on-select' なら kill-ring へ。`M-w' は選択があればコピー、無ければ端末へ送る
;; - module → Lisp はタイマー（`embterm-poll-interval'）でポーリングする（タイトル・カレントディレクトリ・
;;   クリック・プロセスの終了）。module の関数は待たない
;; - module は一時ディレクトリにコピーしてから読む（読み込み済みの DLL は差し替えられないため。T-030）。
;;   新しい版を使うには Emacs の再起動が要る
;; - 複数の端末（T-042）: `embterm'（あれば切り替え、前置引数で新規）、`embterm-new'、`embterm-project'
;;   （project.el のルートで開き、プロジェクトごとに使い回す）、`embterm-list-buffers'
;; - 色（T-034）: `embterm-follow-theme' なら Emacs のテーマ（`default' `cursor' `region' と `ansi-color-*' の face）に
;;   追従し、`enable-theme-functions' / `disable-theme-functions' で開いている端末に反映する
;; - カーソル（T-035）: `embterm-cursor-style' / `embterm-cursor-blink'。アプリの DECSCUSR が優先
;; - OSC（T-039）: 52 の書き込みは kill-ring へ（読み取りは既定で拒む）、9 / 777 は `embterm-notification-functions'、
;;   9;4 は `embterm-progress-functions'、8 のハイパーリンクは Ctrl+クリックで開く。ベル（T-040）は間引く
;; - tr-ime の再変換・前後参照は、端末のバッファでは空の文字列を渡す（T-083）
;; - フォント（T-033）: `embterm-font-family'（候補のリストも可）・`embterm-font-size'・`embterm-font-fallback-families'
;;   （主フォントに無い文字に Windows のフォールバックより先に使う）。`text-scale-adjust' でバッファごとに拡大・縮小

;;; Code:

(require 'ansi-color)
(require 'cl-lib)
(require 'seq)
(require 'subr-x)
(require 'url-util)

(declare-function embterm-module-version "embterm-module")
(declare-function embterm-module-set-log "embterm-module")
(declare-function embterm-module-create "embterm-module")
(declare-function embterm-module-place "embterm-module")
(declare-function embterm-module-hide "embterm-module")
(declare-function embterm-module-remove "embterm-module")
(declare-function embterm-module-select "embterm-module")
(declare-function embterm-module-start "embterm-module")
(declare-function embterm-module-reset-keyboard "embterm-module")
(declare-function embterm-module-send-string "embterm-module")
(declare-function embterm-module-send-key "embterm-module")
(declare-function embterm-module-paste "embterm-module")
(declare-function embterm-module-paste-safe-p "embterm-module")
(declare-function embterm-module-scroll "embterm-module")
(declare-function embterm-module-set-scrollback "embterm-module")
(declare-function embterm-module-selection "embterm-module")
(declare-function embterm-module-clear-selection "embterm-module")
(declare-function embterm-module-request-copy "embterm-module")
(declare-function embterm-module-take-copy "embterm-module")
(declare-function embterm-module-set-font "embterm-module")
(declare-function embterm-module-set-narrow-wide "embterm-module")
(declare-function embterm-module-focus "embterm-module")
(declare-function embterm-module-screenshot "embterm-module")
(declare-function embterm-module-close "embterm-module")
(declare-function embterm-module-shutdown "embterm-module")
(declare-function embterm-module-poll "embterm-module")
(declare-function embterm-module-cursor "embterm-module")
(declare-function embterm-module-text "embterm-module")
(declare-function embterm-module-set-colors "embterm-module")
(declare-function embterm-module-set-cursor "embterm-module")
(declare-function embterm-module-set-clipboard-policy "embterm-module")
(declare-function project-root "project" (project))
(defvar text-scale-mode-amount)
(defvar text-scale-mode-step)
(declare-function tr-ime-modadv--notify-reconvert-string "tr-ime-modadv" (arg1 arg2 arg3))

;;;; 設定

(defgroup embterm nil
  "Emacs のウィンドウに重ねて表示する端末。"
  :group 'terminals
  :prefix "embterm-")

(defconst embterm--this-directory
  (file-name-directory (or load-file-name buffer-file-name default-directory))
  "このファイルのあるディレクトリ。")

(defcustom embterm-module-directory embterm--this-directory
  "embterm_module.dll のあるディレクトリ。
既定はこのファイル（embterm.el）と同じディレクトリ（`cargo xtask dist' の配布物の形）。"
  :type 'directory)

(defcustom embterm-conpty-directory nil
  "conpty.dll（と x64/OpenConsole.exe）のあるディレクトリ。
nil なら `embterm-module-directory'。見つからなければ OS 同梱の ConPTY を使う。"
  :type '(choice (const :tag "module と同じ" nil) directory))

(defcustom embterm-shell
  (if (executable-find "pwsh") '("pwsh" "-NoLogo") '("cmd.exe"))
  "端末で起動するコマンド。文字列のリスト（コマンドと引数）か、1 本の文字列。"
  :type '(choice (repeat string) string))

(defun embterm--set-and-apply (sym value)
  "SYM を VALUE にし、開いている端末にも反映する。"
  (set-default sym value)
  (when (fboundp 'embterm--apply-settings)
    (embterm--apply-settings)))

(defcustom embterm-font-family nil
  "端末のフォント。ファミリ名か、ファミリ名のリスト（前から順に、見つかったものを使う）。
どれも無ければ module の既定（HackGen Console NF → Cascadia Mono → Consolas）。nil なら既定だけ。
セルの大きさはこのフォントの `M' の幅と行の高さで決まるので、等幅のフォントを指定する。"
  :type '(choice (const :tag "既定" nil) string (repeat string))
  :set #'embterm--set-and-apply)

(defcustom embterm-font-size nil
  "端末のフォントの大きさ（ポイント）。nil なら module の既定（`embterm--default-font-size'）。
端末のバッファで `text-scale-adjust'（\\[text-scale-adjust]）すると、そのバッファだけ拡大・縮小する。"
  :type '(choice (const :tag "既定" nil) number)
  :set #'embterm--set-and-apply)

(defcustom embterm-font-fallback-families nil
  "主フォントに無い文字に使うファミリ名のリスト（前から順に。T-033）。
どれにも無い文字は Windows のフォールバック（日本語・記号・色付きの絵文字）に任せる。
例: \\='(\"Symbols Nerd Font Mono\" \"BIZ UDGothic\")。無いファミリは飛ばす。"
  :type '(repeat string)
  :set #'embterm--set-and-apply)

(defconst embterm--default-font-size 12
  "module の既定のフォントの大きさ（ポイント）。`text-scale-mode' の倍率を掛ける元に使う。")

(defcustom embterm-narrow-wide 'overlap
  "幅 1 と数えたのにグリフが広い文字（`①' `★' `※' など）の描き方（T-013）。
`overlap' は縮めずに右隣へはみ出させる（WezTerm と同じ見え方）、`shrink' は 1 セルに縮める（ghostel と同じ）。"
  :type '(choice (const overlap) (const shrink))
  :set #'embterm--set-and-apply)

(defcustom embterm-ime-position t
  "non-nil なら、IME の変換ウィンドウを端末のカーソルの位置に出す（T-010）。
tr-ime advanced（`tr-ime-modadv--set-composition-window'）があるときだけ効く。
無ければ変換ウィンドウは Emacs のポイント（隠れたバッファ）の位置に出る。"
  :type 'boolean)

(defcustom embterm-scrollback-lines 10000
  "端末のスクロールバックの行数の上限。開いている端末にも反映する。"
  :type 'integer
  :set #'embterm--set-and-apply)

(defcustom embterm-copy-on-select t
  "non-nil なら、端末の選択を終えたときに kill-ring とクリップボードへ入れる。"
  :type 'boolean)

(defcustom embterm-confirm-unsafe-paste t
  "non-nil なら、改行を含む文字列を、ブラケットペーストでないときに貼り付ける前に確かめる。"
  :type 'boolean)

(defcustom embterm-background-size '(80 . 24)
  "どのウィンドウにも出さずに開いた端末（`embterm-run' の DISPLAY が `ignore' など）の桁数と行数。
バッファを出すと、そのウィンドウの大きさに合わせ直す（T-082）。"
  :type '(cons integer integer))

(defcustom embterm-kill-buffer-on-exit t
  "non-nil なら、プロセスが終わったら端末のバッファを消す。"
  :type 'boolean)

(defcustom embterm-kill-emacs-timeout 0.5
  "Emacs を終えるとき、端末の中のアプリが穏やかに終わるのを待つ秒数。
アプリに端末を閉じたこと（CTRL_CLOSE_EVENT）を知らせ、この秒数まで待ってから残ったものを止める。
その間は Emacs の終了が待たされる。0 なら待たずに止める（アプリは知らせを受けずに止まる）。"
  :type 'number)

(defcustom embterm-buffer-name-format "*embterm: %s*"
  "タイトル（OSC 0 / 2）をバッファ名にするときの書式。nil ならバッファ名を変えない。
`embterm-default-buffer-name' が使う。"
  :type '(choice (const :tag "変えない" nil) string))

(defcustom embterm-buffer-name-function #'embterm-default-buffer-name
  "端末のバッファ名を決める関数（T-080）。
タイトル（OSC 0 / 2。まだ無ければ nil）を引数に、端末のバッファで呼ばれ、バッファ名を返す。
nil を返せば名前を変えない。同じ名前のバッファがあれば `<2>' などを付ける（`rename-buffer' の UNIQUE）。"
  :type 'function)

(defcustom embterm-buffer-name-max-length 40
  "`embterm-default-buffer-name' が作るバッファ名の長さ（表示の幅）の上限。"
  :type 'integer)

;;;; 色・カーソル・OSC・ベルの設定（T-034 / T-035 / T-039 / T-040）

(defun embterm--set-and-apply-term (sym value)
  "SYM を VALUE にし、開いている端末の色・カーソル・クリップボードの許可に反映する。"
  (set-default sym value)
  (when (fboundp 'embterm--apply-term-settings-all)
    (embterm--apply-term-settings-all)))

(defcustom embterm-follow-theme t
  "non-nil なら、端末の色を Emacs のテーマに追従させる（T-034）。
前景・背景は `default' の face、カーソルは `cursor'、選択は `region'、16 色は `ansi-color-black' などの
`ansi-color-*' の face から取る。テーマを替えると（`enable-theme-functions'）開いている端末にも反映する。
nil なら `embterm-color-foreground' などの値を使う。どちらでも、アプリが OSC で変えた色はそのまま。"
  :type 'boolean
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-foreground nil
  "`embterm-follow-theme' が nil のときの前景色（色の名前か \"#rrggbb\"）。nil なら module の既定。"
  :type '(choice (const :tag "既定" nil) color)
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-background nil
  "`embterm-follow-theme' が nil のときの背景色。nil なら module の既定。"
  :type '(choice (const :tag "既定" nil) color)
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-cursor nil
  "`embterm-follow-theme' が nil のときのカーソルの色。nil なら module の既定。"
  :type '(choice (const :tag "既定" nil) color)
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-selection nil
  "`embterm-follow-theme' が nil のときの選択の背景色。nil なら module の既定。"
  :type '(choice (const :tag "既定" nil) color)
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-selection-foreground nil
  "`embterm-follow-theme' が nil のときの選択の文字の色。nil ならセルの色のまま。"
  :type '(choice (const :tag "セルの色" nil) color)
  :set #'embterm--set-and-apply-term)

(defcustom embterm-color-palette nil
  "`embterm-follow-theme' が nil のときの 16 色（黒・赤・緑・黄・青・マゼンタ・シアン・白、続いて明るい 8 色）。
16 個までの色のリスト。nil の色は module（libghostty-vt）の組み込みの色。"
  :type '(repeat (choice (const :tag "組み込み" nil) color))
  :set #'embterm--set-and-apply-term)

(defcustom embterm-cursor-style 'block
  "カーソルの形（T-035）。アプリが DECSCUSR で形を決めたら、そちらが優先する。"
  :type '(choice (const block) (const bar) (const underline))
  :set #'embterm--set-and-apply-term)

(defcustom embterm-cursor-blink nil
  "non-nil ならカーソルを点滅させる（アプリが DECSCUSR で決めたら、そちらが優先する）。
点滅は、端末のウィンドウを選んでいてフレームにフォーカスがあるときだけ。間隔は Windows の設定（キャレットの点滅）。"
  :type 'boolean
  :set #'embterm--set-and-apply-term)

(defcustom embterm-allow-clipboard-write t
  "non-nil なら、アプリが OSC 52 で書いたテキストを kill-ring（とクリップボード）へ入れる（T-039）。"
  :type 'boolean
  :set #'embterm--set-and-apply-term)

(defcustom embterm-allow-clipboard-read nil
  "non-nil なら、アプリが OSC 52 でクリップボードを読むのを許す（T-039）。
許すと、端末で動くプログラム（ssh の先を含む）がクリップボードの中身を読めるので、既定では拒む。"
  :type 'boolean
  :set #'embterm--set-and-apply-term)

(defcustom embterm-notification-functions (list #'embterm-notification-default)
  "アプリのデスクトップ通知（OSC 9 / 777）を受けたときに呼ぶ関数（T-039）。
引数は TITLE（無ければ空文字列）、BODY、BUFFER（端末のバッファ）。
用意してあるもの: `embterm-notification-default'（既定。Emacs が裏ならトースト、前なら `message'）、
`embterm-notification-message'、`embterm-notification-toast'。"
  :type 'hook)

(defcustom embterm-notification-toast-seconds 10
  "`embterm-notification-toast' の通知を消すまでの秒数（T-099）。
Emacs の通知（`w32-notification-notify'）は消すまで通知領域にアイコンが残るので、この秒数で消す。"
  :type 'number)

(defcustom embterm-show-progress t
  "non-nil なら、アプリの進捗（OSC 9;4）をモードラインに出す（T-100）。"
  :type 'boolean)

(defcustom embterm-progress-functions nil
  "アプリの進捗（OSC 9;4）を受けたときに呼ぶ関数（T-039）。
引数は STATE、PERCENT（0〜100 か nil）、BUFFER。STATE は `set' `remove'
`error' `indeterminate' `pause' のどれか。"
  :type 'hook)

(defcustom embterm-hyperlink-schemes '("http" "https" "ftp" "mailto" "file")
  "Ctrl+クリックで開くハイパーリンク（OSC 8）の URI のスキーム。ほかのものは開かない。"
  :type '(repeat string))

(defcustom embterm-hyperlink-function #'embterm-open-hyperlink
  "Ctrl+クリックしたハイパーリンク（OSC 8）の URI を引数に呼ぶ関数（T-039）。"
  :type 'function)

(defcustom embterm-bell-function #'ding
  "ベル（BEL）で呼ぶ関数（T-040）。1 つの引数 t で呼ぶ（`ding' の引数。キーボードマクロを止めない）。
nil ならベルを鳴らさない。"
  :type '(choice (const :tag "鳴らさない" nil) function))

(defcustom embterm-bell-interval 0.2
  "ベルを鳴らす間隔の下限（秒）。これより短い間隔のベルは捨てる（連打で Emacs を詰まらせない）。"
  :type 'number)

(defcustom embterm-poll-interval 0.05
  "module の状態を読む間隔（秒）。端末があるときだけ動く。
クリックしてからウィンドウが選ばれるまでの遅れになる（キー入力と表示の遅れには関わらない）。"
  :type 'number)

(defcustom embterm-keymap-exceptions
  '("C-c" "C-x" "C-u" "C-h" "M-x" "M-:" "C-\\")
  "semi-char モードで端末に送らず、Emacs に残すキー（ghostel と同じ既定）。
変えたら `embterm-rebuild-keymap' を呼ぶ（customize なら自動）。"
  :type '(repeat string)
  :set (lambda (sym value)
         (set-default sym value)
         (when (fboundp 'embterm-rebuild-keymap)
           (embterm-rebuild-keymap))))

;;;; module の読み込み（T-030）

(defvar embterm--module-loaded nil
  "読み込んだ module の版。nil ならまだ読み込んでいない。")

(defun embterm--native-path (dir)
  "DIR を `\\' 区切りの絶対パスにする（Windows の API に渡すため）。ドライブレターは大文字にする。
Emacs は `~' の展開や `directory-file-name' でドライブレターを小文字に落とすが、子の作業
ディレクトリはほかの端末と同じ大文字にそろえる（プロンプトの表示や、claude が cwd を
プロジェクトの名前に使うため）。"
  (let ((path (subst-char-in-string ?/ ?\\ (directory-file-name (expand-file-name dir)))))
    (if (string-match-p "\\`[a-z]:" path)
        (concat (upcase (substring path 0 1)) (substring path 1))
      path)))

(defun embterm--load-module ()
  "module を読み込む（一度だけ）。
Emacs は module をアンロードできず、読み込み中の DLL はロックされるので、更新時刻とサイズを
名前に入れて一時ディレクトリにコピーしてから読む（GenPreview と同じ）。こうすれば元の DLL を
ビルドし直して上書きできる。conpty.dll と OpenConsole.exe は元の場所のものを使う。"
  (unless embterm--module-loaded
    (unless (and (eq system-type 'windows-nt) module-file-suffix)
      (user-error "embterm: Windows の、module に対応した Emacs が要ります"))
    (let ((src (expand-file-name "embterm_module.dll" embterm-module-directory)))
      (unless (file-exists-p src)
        (user-error "embterm: module が見つかりません: %s（`embterm-module-directory' を設定してください）" src))
      (let* ((attrs (file-attributes src))
             (stamp (format "%s-%d"
                            (format-time-string "%Y%m%d%H%M%S" (file-attribute-modification-time attrs))
                            (file-attribute-size attrs)))
             (copy (expand-file-name (format "embterm_module-%s.dll" stamp) temporary-file-directory)))
        ;; 以前の版のコピーを片付ける（読み込み中のものは消せないので無視する）
        (dolist (old (directory-files temporary-file-directory t "\\`embterm_module-.*\\.dll\\'"))
          (unless (equal old copy)
            (ignore-errors (delete-file old))))
        (unless (file-exists-p copy)
          (copy-file src copy t))
        (module-load copy)
        (setq embterm--module-loaded (embterm-module-version))))))

;;;; バッファの状態

(cl-defstruct (embterm--pane (:constructor embterm--make-pane (window id)))
  "Emacs のウィンドウ 1 つに対応するペイン（子 HWND 1 枚。T-063）。"
  window id
  (placed nil))

(defvar-local embterm--term nil "module の端末（user-ptr）。")
(defvar-local embterm--panes nil "このバッファのペイン（`embterm--pane' のリスト）。")
(defvar-local embterm--next-pane 1 "次に作るペインの番号。")
(defvar-local embterm--selected-pane 'unknown "最後に module に伝えた選択ペインの番号（nil は選んでいない）。")
(defvar-local embterm--pane-frame nil "ペインを置いているフレーム。")
(defvar-local embterm--generation nil "最後に読んだ module の状態の世代。")
(defvar-local embterm--state nil "最後に読んだ module の状態（plist）。")
(defvar-local embterm--title nil "最後に反映したタイトル。")
(defvar-local embterm--focused 'unknown "最後に module に伝えたフォーカスの状態。")
(defvar-local embterm--exited nil "プロセスが終わったか。")
(defvar-local embterm--progress nil "アプリの進捗（OSC 9;4）の (STATE . PERCENT)。無ければ nil（T-100）。")
(defvar-local embterm--input-mode 'semi-char
  "入力モード。`semi-char'（キーを端末に送る）か `copy'（キーを Emacs に返し、スクロールバックごと写す）。")
(defvar-local embterm--copy-window nil "copy モードに入ったときに選んでいたウィンドウ。")
(defvar-local embterm--copy-filling nil
  "copy モードのテキストを受け取っている途中なら (TOP POINT WRAPS)。最初の一部で erase してから足していく（T-086）。")
(defvar-local embterm--semi-char nil
  "non-nil なら `embterm-semi-char-map' が効く（`emulation-mode-map-alists' の条件）。")
(defvar-local embterm--command nil "起動したコマンド（argv の先頭）。タイトルが無いときのバッファ名に使う。")
(defvar-local embterm--project nil "`embterm-project' で開いた端末なら、そのプロジェクトのルート。")
(defvar-local embterm--fixed-name nil
  "non-nil なら、バッファ名をタイトルで付け替えない（`embterm-run' の NAME を指定したとき）。")

(defvar embterm--buffers nil "端末のバッファ。")
(defvar embterm--timer nil "module の状態を読むタイマー。")

(defun embterm--live-buffers ()
  (setq embterm--buffers
        (seq-filter (lambda (b) (and (buffer-live-p b) (buffer-local-value 'embterm--term b)))
                    embterm--buffers)))

;;;; 起動

(defun embterm--argv ()
  (let ((argv (if (stringp embterm-shell) (split-string-and-unquote embterm-shell) embterm-shell)))
    (unless (and argv (seq-every-p #'stringp argv))
      (user-error "embterm: `embterm-shell' が正しくありません: %S" embterm-shell))
    argv))

(defun embterm--cwd ()
  "子プロセスの作業ディレクトリ（リモートなら家のディレクトリ）。"
  (let ((dir (if (file-remote-p default-directory) "~" default-directory)))
    (embterm--native-path dir)))

(defun embterm--conpty-dir ()
  (embterm--native-path (or embterm-conpty-directory embterm-module-directory)))

(defun embterm--env-name (entry)
  "`process-environment' の項目 ENTRY（\"NAME=VALUE\" か \"NAME\"）の名前。
Windows の \"=C:=C:\\\" のように `=' で始まる名前があるので、2 文字目以降の最初の `=' までとする。"
  (let ((i (and (> (length entry) 1) (string-search "=" entry 1))))
    (if i (substring entry 0 i) entry)))

(defun embterm--environment-changes (&optional env)
  "ENV（既定は `process-environment'）が Emacs の起動時の環境（`initial-environment'）から
変わったところを、`embterm-module-create' に渡す文字列のベクタにする（T-109）。
\"NAME=VALUE\" は設定、\"NAME\" は削除。子プロセスの環境は Emacs のプロセスの環境に
これを当てたもの（`make-process' と同じく `setenv' が子に届く）。

変わっていない項目は渡さない。`process-environment' には起動時の ANSI の文字列が
復号されないまま残る項目があり、往復させると化けるため。"
  (let ((seen (make-hash-table :test #'equal))
        changes)
    (dolist (e (or env process-environment))
      (let* ((name (embterm--env-name e))
             (key (upcase name)))
        (unless (gethash key seen)
          (puthash key t seen)
          (cond
           ;; "NAME" は削除（`setenv' に nil を渡したとき）
           ((equal name e)
            (when (getenv-internal name initial-environment)
              (push name changes)))
           ((not (member e initial-environment))
            (push e changes))))))
    ;; 起動時にはあったのに、ENV に名前ごと無いもの（ENV を組み立て直したとき）も削除
    (dolist (e initial-environment)
      (let ((name (embterm--env-name e)))
        (unless (gethash (upcase name) seen)
          (puthash (upcase name) t seen)
          (push name changes))))
    ;; `setenv' は名前と値を `locale-coding-system' で符号化して入れる（ユニバイトの文字列）。
    ;; module には UTF-8 の文字列で渡すので、同じ符号化で戻す
    (vconcat (mapcar (lambda (e)
                       (if (or (multibyte-string-p e) (string-match-p "\\`[[:ascii:]]*\\'" e)
                               (null locale-coding-system))
                           e
                         (decode-coding-string e locale-coding-system)))
                     (nreverse changes)))))

(cl-defun embterm--create (dir &key project argv environment name display)
  "DIR で ARGV（nil なら `embterm-shell'）を起動する端末のバッファを作って出す。バッファを返す。
PROJECT はプロジェクトのルート（T-042）。ENVIRONMENT は子の環境（`process-environment' の形。
nil なら `process-environment'）。NAME はバッファ名（nil ならタイトルから付ける）。
DISPLAY はバッファを出す関数（nil なら `pop-to-buffer-same-window'）。"
  (embterm--load-module)
  (let* ((default-directory dir)
         (argv (or argv (embterm--argv)))
         (env (embterm--environment-changes environment))
         (buf (generate-new-buffer (or name "*embterm*"))))
    (with-current-buffer buf
      (setq default-directory dir)
      (embterm-mode)
      (setq embterm--command (car argv)
            embterm--project project
            embterm--fixed-name (and name t))
      (condition-case err
          (setq embterm--term
                (apply #'embterm-module-create (vconcat argv) (embterm--cwd) (embterm--conpty-dir)
                       (append (embterm--font-args)
                               (list (symbol-name embterm-narrow-wide)
                                     embterm-scrollback-lines
                                     env))))
        (error (kill-buffer buf)
               (signal (car err) (cdr err))))
      ;; 子プロセスは最初の place で起動する。その前に色・カーソル・クリップボードの許可を積む（頼みごとは順に片付く）
      (embterm--apply-term-settings (embterm--color-args))
      (push buf embterm--buffers)
      (embterm--rename-buffer nil))
    (funcall (or display #'pop-to-buffer-same-window) buf)
    (with-current-buffer buf
      (embterm--activate)
      (embterm--sync)
      ;; どのウィンドウにも出さなかった（`ignore' など）なら、表示を待たずに起動する（T-082）
      (unless (get-buffer-window buf 'visible)
        (embterm-module-start embterm--term (car embterm-background-size) (cdr embterm-background-size))))
    buf))

;;;###autoload
(cl-defun embterm-run (command &key directory environment name display)
  "COMMAND（プログラムと引数の文字列のリスト）を端末で起動し、そのバッファを返す（T-110）。
DIRECTORY は作業ディレクトリ（nil なら `default-directory'）。
ENVIRONMENT は子の環境で、`process-environment' と同じ形（nil なら `process-environment'）。
`process-environment' を `copy-sequence' して `setenv' で X を消したものを渡せば、
X を消して起動する。
NAME はバッファ名（同じ名前があれば `<2>' などを付ける）。指定するとタイトルで付け替えない。
DISPLAY はバッファを出す関数（nil なら `pop-to-buffer-same-window'。たとえば `pop-to-buffer'）。
`ignore' のようにどのウィンドウにも出さなければ、`embterm-background-size' の大きさで裏で起動し、
後でバッファを出したときにウィンドウの大きさに合わせる（T-082）。
ヘッダ行などは、返ったバッファで設定すればよい（端末の大きさはウィンドウに合わせ直す）。"
  (unless (and (consp command) (seq-every-p #'stringp command))
    (signal 'wrong-type-argument (list 'list-of-strings command)))
  (embterm--create (file-name-as-directory (expand-file-name (or directory default-directory)))
                   :argv command :environment environment :name name :display display))

(defun embterm--buffers-by-recency ()
  "端末のバッファを、最近選んだ順に。"
  (let ((live (embterm--live-buffers)))
    (seq-filter (lambda (b) (memq b live)) (buffer-list))))

;;;###autoload
(defun embterm (&optional arg)
  "端末のバッファに切り替える。無ければ新しく開く（`embterm-shell' を `default-directory' で起動する）。
いま端末のバッファにいれば、ほかの端末のうち最近選んだものへ切り替える。
前置引数 ARG があれば、必ず新しく開く（`embterm-new' と同じ）。バッファを返す。"
  (interactive "P")
  (let* ((bufs (embterm--buffers-by-recency))
         (existing (and (not arg)
                        (or (seq-find (lambda (b) (not (eq b (current-buffer)))) bufs)
                            (car bufs)))))
    (if existing
        (progn (pop-to-buffer-same-window existing)
               existing)
      (embterm--create default-directory))))

;;;###autoload
(defun embterm-new ()
  "新しい端末のバッファを開く（`embterm-shell' を `default-directory' で起動する）。バッファを返す。"
  (interactive)
  (embterm--create default-directory))

;;;###autoload
(defun embterm-project (&optional arg)
  "今のプロジェクト（project.el）のルートで端末を開く。そのプロジェクトの端末があれば切り替える。
前置引数 ARG があれば、必ず新しく開く。バッファを返す。"
  (interactive "P")
  (require 'project)
  (let* ((root (file-name-as-directory (expand-file-name (project-root (project-current t)))))
         (existing (and (not arg)
                        (seq-find (lambda (b) (equal (buffer-local-value 'embterm--project b) root))
                                  (embterm--buffers-by-recency)))))
    (if existing
        (progn (pop-to-buffer-same-window existing)
               existing)
      (embterm--create root :project root))))

(defun embterm--annotate-buffer (name)
  "`embterm-list-buffers' の候補 NAME の注釈（ディレクトリ）。"
  (when-let* ((buf (get-buffer name)))
    (concat (propertize " " 'display '(space :align-to 40))
            (abbreviate-file-name (buffer-local-value 'default-directory buf)))))

;;;###autoload
(defun embterm-list-buffers ()
  "端末のバッファを選んで切り替える（`completing-read'）。"
  (interactive)
  (let ((bufs (embterm--buffers-by-recency)))
    (unless bufs
      (user-error "embterm: 端末のバッファがありません"))
    (let* ((names (mapcar #'buffer-name bufs))
           (default (or (seq-find (lambda (n) (not (equal n (buffer-name)))) names) (car names)))
           (completion-extra-properties (list :annotation-function #'embterm--annotate-buffer))
           (choice (completing-read (format-prompt "端末" default) names nil t nil nil default)))
      (pop-to-buffer-same-window (get-buffer choice)))))

(defun embterm--placeholder ()
  (let ((inhibit-read-only t))
    (erase-buffer)
    (insert (propertize "EmbTerm: 端末はこのウィンドウに重ねて表示しています。\n"
                        'face 'shadow)
            (propertize (substitute-command-keys
                         "\\<embterm-mode-map>\\[embterm-copy-mode] で画面のテキストをこのバッファに写して Emacs のキーで操作し、\\[embterm-semi-char-mode] で戻ります。\n")
                        'face 'shadow))
    (goto-char (point-min))))

(defvar-keymap embterm-mode-map
  :doc "`embterm-mode' のキーマップ。semi-char モードでも C-c の後のキーはここで受ける。"
  "C-c C-c" #'embterm-send-C-c
  "C-c C-z" #'embterm-send-C-z
  "C-c C-d" #'embterm-send-C-d
  "C-c C-\\" #'embterm-send-C-backslash
  "C-c C-j" #'embterm-semi-char-mode
  "C-c C-k" #'embterm-copy-mode
  "C-c C-y" #'embterm-yank)

(define-derived-mode embterm-mode fundamental-mode "EmbTerm"
  "EmbTerm の端末のバッファ。バッファの中身は置き物で、端末は子 HWND が描く。

\\{embterm-mode-map}"
  :interactive nil
  (setq buffer-read-only t
        truncate-lines t
        cursor-type nil
        embterm--input-mode 'semi-char
        embterm--semi-char t)
  (setq-local mode-line-process '(:eval (embterm--mode-line)))
  (setq-local filter-buffer-substring-function #'embterm--filter-buffer-substring)
  (embterm--placeholder)
  (embterm--emulation-first)
  (embterm--tr-ime-setup)
  (add-hook 'text-scale-mode-hook #'embterm--apply-font nil t)
  (add-hook 'kill-buffer-hook #'embterm--close nil t))

;;;; tr-ime の再変換・前後参照（T-083。docs/research/emacs-ime.md §3.5）

(defun embterm--tr-ime-notify-empty ()
  "tr-ime の再変換（RECONVERSION）・前後参照（DOCUMENTFEED）に空の文字列を渡す。
端末のバッファの中身は置き物なので、ポイントの行を IME に渡さない。再変換で `delete-char' も走らない。
tr-ime の C++ 側は通知を待つので、何も通知しないと時間切れまで止まる。"
  (when (fboundp 'tr-ime-modadv--notify-reconvert-string)
    (tr-ime-modadv--notify-reconvert-string
     (string-to-number (frame-parameter nil 'window-id)) "" 0)))

(defun embterm--tr-ime-setup ()
  "端末のバッファでは、tr-ime の再変換・前後参照のフックをバッファローカルに差し替える。
ローカルの値に t を入れないので、グローバルのフック（ポイントの行を渡すもの）は呼ばれない。
tr-ime advanced が無い（フックの変数が無い）ときは何もしない。"
  (dolist (hook '(tr-ime-modadv--reconvertstring-hook tr-ime-modadv--documentfeed-hook))
    (when (boundp hook)
      (set (make-local-variable hook) (list #'embterm--tr-ime-notify-empty)))))

(defun embterm--mode-line ()
  (concat (if (eq embterm--input-mode 'copy) " [copy]" "")
          (when embterm--exited " [終了]")
          (when (and embterm-show-progress embterm--progress) (embterm--progress-text))))

(defun embterm--set-progress (state percent)
  "アプリの進捗（OSC 9;4）を覚えてモードラインを更新する（T-100）。`remove' で消す。"
  (let ((new (unless (eq state 'remove) (cons state percent))))
    (unless (equal new embterm--progress)
      (setq embterm--progress new)
      (force-mode-line-update))))

(defun embterm--progress-text ()
  "モードラインの進捗の表示（` [42%]' など）。"
  (pcase-let ((`(,state . ,percent) embterm--progress))
    (let ((pct (if percent (format "%d%%" percent) "")))
      (pcase state
        ('set (propertize (format " [%s]" pct) 'face 'success))
        ('indeterminate (propertize " […]" 'face 'success))
        ('error (propertize (format " [エラー %s]" pct) 'face 'error))
        ('pause (propertize (format " [一時停止 %s]" pct) 'face 'warning))
        (_ "")))))

;;;; 閉じる

(defun embterm--close ()
  "このバッファの端末を閉じる（待たない。PTY の後始末は module が裏で行う）。"
  (when embterm--term
    (ignore-errors (embterm-module-close embterm--term))
    (setq embterm--term nil))
  (setq embterm--buffers (delq (current-buffer) embterm--buffers)))

(defun embterm--close-all ()
  "Emacs を終えるときに、すべての端末を閉じる（`kill-emacs-hook'）。
`embterm-kill-emacs-timeout' 秒までアプリが穏やかに終わるのを待つ（T-076）。"
  (when (and embterm--module-loaded (fboundp 'embterm-module-shutdown) (> embterm-kill-emacs-timeout 0))
    (ignore-errors (embterm-module-shutdown (round (* 1000 embterm-kill-emacs-timeout)))))
  (dolist (buf (copy-sequence (embterm--live-buffers)))
    (with-current-buffer buf
      (embterm--close))))

;;;; 位置合わせ

(defun embterm--activate ()
  "位置合わせのフックとポーリングのタイマーを（まだなら）始める。"
  (add-hook 'window-size-change-functions #'embterm--sync)
  (add-hook 'window-buffer-change-functions #'embterm--sync)
  (add-hook 'window-selection-change-functions #'embterm--sync)
  (add-function :after after-focus-change-function #'embterm--update-focus)
  (add-hook 'kill-emacs-hook #'embterm--close-all)
  (unless embterm--timer
    (setq embterm--timer (run-with-timer embterm-poll-interval embterm-poll-interval #'embterm--poll))))

(defun embterm--deactivate ()
  (embterm--ime-reset)
  (remove-hook 'isearch-mode-end-hook #'embterm--ime-forget)
  (remove-hook 'window-size-change-functions #'embterm--sync)
  (remove-hook 'window-buffer-change-functions #'embterm--sync)
  (remove-hook 'window-selection-change-functions #'embterm--sync)
  (remove-function after-focus-change-function #'embterm--update-focus)
  (when embterm--timer
    (cancel-timer embterm--timer)
    (setq embterm--timer nil)))

(defun embterm--sync (&rest _)
  "すべての端末を、いまバッファを出しているウィンドウに合わせる。"
  (dolist (buf (copy-sequence (embterm--live-buffers)))
    (with-current-buffer buf
      (condition-case err
          (embterm--place)
        (error (message "embterm: %s" (error-message-string err))))))
  (embterm--update-focus)
  (unless embterm--buffers
    (embterm--deactivate)))

(defun embterm--windows ()
  "ペインを置くウィンドウのリスト。
別のフレームに同じバッファを出しているときは 1 つのフレームのウィンドウだけ
（選んでいるウィンドウのフレーム → 前に置いていたフレーム → 最初のウィンドウのフレーム）。要件 T-016。"
  (let* ((all (get-buffer-window-list (current-buffer) nil 'visible))
         (frame (cond ((memq (selected-window) all) (selected-frame))
                      ((and (frame-live-p embterm--pane-frame)
                            (seq-find (lambda (w) (eq (window-frame w) embterm--pane-frame)) all))
                       embterm--pane-frame)
                      (all (window-frame (car all))))))
    (setq embterm--pane-frame frame)
    (seq-filter (lambda (w) (eq (window-frame w) frame)) all)))

(defun embterm--pane-of (window)
  "WINDOW のペイン（このバッファの）。"
  (seq-find (lambda (p) (eq (embterm--pane-window p) window)) embterm--panes))

(defun embterm--pane-by-id (id)
  "番号 ID のペイン。"
  (seq-find (lambda (p) (eql (embterm--pane-id p) id)) embterm--panes))

(defun embterm--place-pane (pane)
  "ペイン PANE を、そのウィンドウの本文の位置に置く（同じ位置なら送らない）。"
  (let* ((win (embterm--pane-window pane))
         (id (frame-parameter (window-frame win) 'window-id))
         (edges (window-inside-pixel-edges win))
         (key (list id edges)))
    (unless (equal key (embterm--pane-placed pane))
      (pcase-let ((`(,l ,tp ,r ,b) edges))
        (embterm-module-place embterm--term (embterm--pane-id pane) id l tp (- r l) (- b tp)))
      (setf (embterm--pane-placed pane) key))))

(defun embterm--hide-pane (pane)
  (when (embterm--pane-placed pane)
    (embterm-module-hide embterm--term (embterm--pane-id pane))
    (setf (embterm--pane-placed pane) nil)))

(defconst embterm--max-parked-panes 4
  "ウィンドウが消えたあとも隠して取っておくペインの数の上限（T-087）。")

(defun embterm--place ()
  "このバッファの端末を、出しているすべてのウィンドウに置く。copy モードの間は隠す（スクロール位置は残る）。
ペインは module がスクロール位置を持つので、ウィンドウが替わってもできるだけ同じペインを使う（T-087）:
- 端末が別のウィンドウへ移った（`window-swap-states' や、バッファの入れ替え）なら、そのペインを新しいウィンドウに付け替える
- ウィンドウが消えた（タブの切り替え・`delete-window'）ペインは隠して取っておき、同じウィンドウが
  `set-window-configuration' で戻ったら（tab-bar がタブに戻るとき・winner-mode）そのまま使う。
  取っておくのは `embterm--max-parked-panes' まで（古いものから消す）"
  (let* ((windows (embterm--windows))
         (gone (seq-remove (lambda (p) (memq (embterm--pane-window p) windows)) embterm--panes))
         ;; 生きているウィンドウから外れたペイン（端末が別のウィンドウへ移った）を先に使う
         (movable (seq-filter (lambda (p) (window-live-p (embterm--pane-window p))) gone)))
    (dolist (win windows)
      (let ((pane (or (embterm--pane-of win)
                      (when-let* ((p (pop movable)))
                        (setq gone (delq p gone))
                        (setf (embterm--pane-window p) win
                              (embterm--pane-placed p) nil)
                        p)
                      (let ((new (embterm--make-pane win embterm--next-pane)))
                        (cl-incf embterm--next-pane)
                        (setq embterm--panes (append embterm--panes (list new)))
                        new))))
        (if (eq embterm--input-mode 'semi-char)
            (embterm--place-pane pane)
          (embterm--hide-pane pane))))
    ;; 残りは、ウィンドウが生きていれば（別のバッファを出した・別のフレームを選んだ）消し、
    ;; 消えたウィンドウのものは隠して取っておく
    (let ((parked 0))
      (dolist (p (reverse gone))
        (if (and (not (window-live-p (embterm--pane-window p)))
                 (< parked embterm--max-parked-panes))
            (progn (cl-incf parked)
                   (embterm--hide-pane p))
          (embterm-module-remove embterm--term (embterm--pane-id p))
          (setq embterm--panes (delq p embterm--panes)))))
    (embterm--send-selected-pane)))

(defun embterm--send-selected-pane ()
  "選んでいるウィンドウのペインを module に伝える（変わったときだけ）。
端末の大きさはこのペインに合わせる。端末のウィンドウを選んでいないときは nil。"
  (let* ((pane (and (eq (window-buffer (selected-window)) (current-buffer))
                    (eq embterm--input-mode 'semi-char)
                    (embterm--pane-of (selected-window))))
         (id (and pane (embterm--pane-id pane))))
    (unless (eq id embterm--selected-pane)
      (setq embterm--selected-pane id)
      (embterm-module-select embterm--term id))))

;;;; フォーカス（T-070）

(defun embterm--update-focus (&rest _)
  "端末のフォーカスの出入りを module に伝える。
選んでいるウィンドウが端末のバッファで、そのフレームにフォーカスがあるときだけ「入った」とする。"
  (let ((focus-ok (not (eq (frame-focus-state (selected-frame)) nil)))
        (selected-buf (window-buffer (selected-window))))
    (dolist (buf (embterm--live-buffers))
      (with-current-buffer buf
        (let ((f (and focus-ok (eq selected-buf buf) (eq embterm--input-mode 'semi-char))))
          (unless (eq f embterm--focused)
            (setq embterm--focused f)
            (ignore-errors (embterm-module-focus embterm--term f))))))))

;;;; IME の変換ウィンドウの位置（T-010。docs/research/emacs-ime.md の A-2）

(declare-function tr-ime-modadv--set-composition-window "tr-ime-modadv"
                  (arg1 arg2 arg3 arg4 arg5 arg6 arg7 arg8))

(defvar embterm--ime-last nil
  "最後に tr-ime に渡した位置の引数。nil なら上書きしていない。同じなら呼び直さない。")

(defun embterm--ime-available-p ()
  (and embterm-ime-position (fboundp 'tr-ime-modadv--set-composition-window)))

(defun embterm--update-ime ()
  "選んでいるウィンドウが端末なら、変換ウィンドウを端末のカーソルに合わせる。
そうでなければ上書きをやめる。カーソルの矩形（子 HWND の中の画素）に、子 HWND を置いた
ウィンドウの本文の左上を足すとフレームの中の画素になる。"
  (when (embterm--ime-available-p)
    (let* ((win (selected-window))
           (args
            (with-current-buffer (window-buffer win)
              (when (and embterm--term
                         (embterm--pane-of win)
                         (eq embterm--input-mode 'semi-char))
                (pcase (ignore-errors (embterm-module-cursor embterm--term))
                  (`(,x ,y ,_w ,_h)
                   (pcase-let ((`(,l ,tp ,r ,b) (window-inside-pixel-edges win)))
                     (list (string-to-number (frame-parameter (window-frame win) 'window-id))
                           1 (+ l x) (+ tp y) l tp r b))))))))
      (cond
       (args
        (unless (equal args embterm--ime-last)
          (setq embterm--ime-last args)
          (add-hook 'isearch-mode-end-hook #'embterm--ime-forget)
          (apply #'tr-ime-modadv--set-composition-window args)))
       (embterm--ime-last (embterm--ime-reset))))))

(defun embterm--ime-reset ()
  "変換ウィンドウの位置の上書きをやめる（Emacs のポイントの位置に戻す）。"
  (when (and embterm--ime-last (fboundp 'tr-ime-modadv--set-composition-window))
    (ignore-errors
      (tr-ime-modadv--set-composition-window (car embterm--ime-last) 0 0 0 0 0 0 0)))
  (setq embterm--ime-last nil))

(defun embterm--ime-forget (&rest _)
  "tr-ime が isearch の終わりに上書きを戻すので、次のポーリングで入れ直させる。"
  (setq embterm--ime-last nil))

;;;; ポーリング

(defun embterm--poll ()
  ;; あとから有効になったモード（`cua-mode' など）に前を取られていたら、取り返す
  (unless (eq (car emulation-mode-map-alists) 'embterm--emulation-alist)
    (embterm--emulation-first))
  (dolist (buf (copy-sequence (embterm--live-buffers)))
    (when (buffer-live-p buf)
      (with-current-buffer buf
        (condition-case err
            (embterm--poll-1)
          (error (message "embterm: %s" (error-message-string err)))))))
  (embterm--update-focus)
  (embterm--update-ime)
  (unless embterm--buffers
    (embterm--deactivate)))

(defun embterm--poll-1 ()
  ;; フックが拾わない位置の変化（あれば）もここで拾う。同じなら module には送らない
  (embterm--place)
  (let ((st (embterm-module-poll embterm--term embterm--generation))
        (buf (current-buffer)))
    (when st
      (setq embterm--generation (plist-get st :generation)
            embterm--state st)
      ;; 親のフレームが消されると子 HWND も消える（端末は残る）。置き直せば作り直される
      (dolist (info (plist-get st :panes))
        (pcase info
          (`(,id 0 . ,_)
           (let ((pane (embterm--pane-by-id id)))
             (when pane (setf (embterm--pane-placed pane) nil))))))
      (embterm--apply-title (plist-get st :title))
      (embterm--apply-pwd (plist-get st :pwd))
      (dolist (e (plist-get st :events))
        ;; (exited) でバッファが消えたら、残りの知らせは捨てる
        (when (and (buffer-live-p buf) (eq (current-buffer) buf))
          (pcase e
            (`(clicked . ,pane) (embterm--select-pane-window pane))
            (`(bell) (embterm--bell))
            (`(exited . ,code) (embterm--exited code))
            (`(screenshot . ,path) (message "embterm: 画面を保存しました: %s" path))
            (`(selected . ,text) (embterm--on-selected text))
            (`(copy-ready) (embterm--fill-copy))
            (`(error . ,m) (message "embterm: %s" m))
            (`(clipboard . ,text) (embterm--on-clipboard text))
            (`(notification ,title . ,body)
             (run-hook-with-args 'embterm-notification-functions title body buf))
            (`(progress ,state . ,percent)
             (embterm--set-progress state percent)
             (run-hook-with-args 'embterm-progress-functions state percent buf))
            (`(link . ,uri) (funcall embterm-hyperlink-function uri))
            (_ nil)))))))

;;;; バッファ名（T-080）

(defun embterm--shorten-title (title)
  "TITLE が長いパス（`C:\\WINDOWS\\SYSTEM32\\cmd.exe' など）なら、ファイル名（拡張子なし）にする。
cmd はコマンドを動かしている間 `パス - コマンド' にするので、` - ' より後ろはそのまま残す。"
  (let* ((sep (string-search " - " title))
         (head (if sep (substring title 0 sep) title))
         (tail (if sep (substring title sep) "")))
    (if (and (> (length head) 1)
             (string-match-p "\\`\\(?:[A-Za-z]:[\\/]\\|[\\/]\\)" head))
        (let ((base (file-name-base (directory-file-name head))))
          (if (string-empty-p base) title (concat base tail)))
      title)))

(defun embterm-default-buffer-name (title)
  "既定のバッファ名（`embterm-buffer-name-function'）。
TITLE が長いパスならファイル名だけ（拡張子なし）、TITLE が無ければ起動したコマンドの名前を
`embterm-buffer-name-format' に入れ、全体を `embterm-buffer-name-max-length' に収める。
`embterm-buffer-name-format' が nil なら nil（名前を変えない）。"
  (when embterm-buffer-name-format
    (let* ((name (cond ((and title (not (string-empty-p title))) (embterm--shorten-title title))
                       (embterm--command (file-name-base embterm--command))
                       (t "embterm")))
           ;; `embterm-project' で開いた端末はプロジェクト名を前に付ける（T-103）
           (name (if embterm--project
                     (format "%s: %s" (file-name-nondirectory (directory-file-name embterm--project)) name)
                   name))
           (room (max 1 (- embterm-buffer-name-max-length
                           (string-width (format embterm-buffer-name-format ""))))))
      (format embterm-buffer-name-format
              (truncate-string-to-width (string-replace "\n" " " name) room nil nil "…")))))

(defun embterm--rename-buffer (title)
  "TITLE（nil はまだ無い）からバッファ名を決めて付ける。同じ名前があれば `<2>' などを付ける。
`embterm-run' で名前を決めたバッファは付け替えない。"
  (unless embterm--fixed-name
    (let ((name (funcall embterm-buffer-name-function title)))
      (when (and (stringp name) (not (string-empty-p name)) (not (equal name (buffer-name))))
        (rename-buffer name t)))))

(defun embterm--apply-title (title)
  (unless (equal title embterm--title)
    (setq embterm--title title)
    (embterm--rename-buffer (and title (not (string-empty-p title)) title))))

(defun embterm--file-uri-path (uri)
  "`file://HOST/PATH' の URI をパスにする（`/C:/Users' は `C:/Users'）。URI でなければそのまま。"
  (if (string-match "\\`file://[^/]*\\(/.*\\)\\'" uri)
      (let ((p (url-unhex-string (match-string 1 uri))))
        (if (string-match-p "\\`/[A-Za-z]:" p) (substring p 1) p))
    uri))

(defun embterm--apply-pwd (pwd)
  "カレントディレクトリ（OSC 7 の `file://HOST/PATH' か、パス）を `default-directory' にする。"
  (when (and pwd (not (string-empty-p pwd)))
    (let ((path (embterm--file-uri-path pwd)))
      (when (file-directory-p path)
        (setq default-directory (file-name-as-directory (expand-file-name path)))))))

;;;; ベル・OSC（T-039 / T-040）

(defvar embterm--last-bell 0 "最後にベルを鳴らした時刻（すべての端末で 1 つ）。")

(defun embterm--bell ()
  "`embterm-bell-function' を呼ぶ。`embterm-bell-interval' より短い間隔のベルは捨てる。
module も、まだ読んでいないベルを 1 つにまとめている。"
  (when embterm-bell-function
    (let ((now (float-time)))
      (when (>= (- now embterm--last-bell) embterm-bell-interval)
        (setq embterm--last-bell now)
        (funcall embterm-bell-function t)))))

(defun embterm--on-clipboard (text)
  "アプリが OSC 52 で書いた TEXT を kill-ring（とクリップボード）へ入れる。"
  (when (and embterm-allow-clipboard-write (stringp text) (not (string-empty-p text))
             (not (equal text (car kill-ring))))
    (kill-new text)))

(defun embterm--unattended-p (buffer)
  "端末 BUFFER のウィンドウが選ばれていないか、フレームにフォーカスが無ければ non-nil。"
  (or (not (eq (window-buffer (selected-window)) buffer))
      (null (frame-focus-state (selected-frame)))))

(defun embterm-notification-text (title body buffer)
  "通知の文面。どの端末から来たか分かるよう、バッファ名を前に付ける。"
  (let ((flat (lambda (s) (string-trim (replace-regexp-in-string "[\n\r\t]+" " " (or s ""))))))
    (format "embterm 通知 [%s] %s%s"
            (if (buffer-live-p buffer) (buffer-name buffer) "?")
            (if (string-empty-p (funcall flat title)) "" (concat (funcall flat title) ": "))
            (funcall flat body))))

(defun embterm-notification-message (title body buffer)
  "通知（OSC 9 / 777）を `message' で出す。
端末のウィンドウが選ばれていないか、フレームにフォーカスが無いときだけ出す。"
  (when (embterm--unattended-p buffer)
    (message "%s" (embterm-notification-text title body buffer))))

(declare-function w32-notification-notify "w32fns.c" (&rest params))
(declare-function w32-notification-close "w32fns.c" (id))

(defvar embterm--toast nil
  "出している通知 (FRAME ID TIMER)。Emacs の通知はフレームごとに 1 つで、作ったフレームを選んで消す。")

(defun embterm--toast-close ()
  "出している通知を消す。"
  (pcase embterm--toast
    (`(,frame ,id ,timer)
     (setq embterm--toast nil)
     (when (timerp timer) (cancel-timer timer))
     (when (frame-live-p frame)
       (with-selected-frame frame
         (ignore-errors (w32-notification-close id)))))))

(defun embterm-notification-toast (title body buffer)
  "通知（OSC 9 / 777）を Windows の通知（トースト）で出す（T-099）。
`embterm-notification-toast-seconds' 秒で消す。前の通知が残っていれば先に消す。
`w32-notification-notify' が無ければ `message'。"
  (if (not (fboundp 'w32-notification-notify))
      (message "%s" (embterm-notification-text title body buffer))
    (embterm--toast-close)
    (let* ((flat (lambda (s) (string-trim (replace-regexp-in-string "[\n\r\t]+" " " (or s "")))))
           (head (format "%s%s" (if (string-empty-p (funcall flat title)) "" (concat (funcall flat title) " — "))
                         (if (buffer-live-p buffer) (buffer-name buffer) "embterm")))
           (id (w32-notification-notify :title (truncate-string-to-width head 63)
                                        :body (truncate-string-to-width (funcall flat body) 255)
                                        :tip "EmbTerm" :level 'info)))
      (when id
        (setq embterm--toast
              (list (selected-frame) id
                    (run-at-time embterm-notification-toast-seconds nil #'embterm--toast-close)))))))

(defun embterm-notification-default (title body buffer)
  "通知（OSC 9 / 777）の既定（`embterm-notification-functions'。T-099）。
Emacs のフレームにフォーカスが無ければトースト（`embterm-notification-toast'）、
フォーカスはあるが端末のウィンドウを選んでいなければ `message'、端末を見ていれば何もしない。"
  (cond ((null (frame-focus-state (selected-frame))) (embterm-notification-toast title body buffer))
        ((embterm--unattended-p buffer) (embterm-notification-message title body buffer))))

(defun embterm-open-hyperlink (uri)
  "Ctrl+クリックしたハイパーリンク（OSC 8）の URI を開く（`embterm-hyperlink-function' の既定）。
スキームが `embterm-hyperlink-schemes' に無いものは開かない。`file:' はシェルで開かずに
Emacs で別のウィンドウに開く（実行ファイルを起動しないため）。ほかは `browse-url'。"
  (let ((scheme (and (string-match "\\`\\([A-Za-z][A-Za-z0-9+.-]*\\):" uri)
                     (downcase (match-string 1 uri)))))
    (cond
     ((not (member scheme embterm-hyperlink-schemes))
      (message "embterm: 開かないリンクです（スキーム %s）: %s" scheme uri))
     ((equal scheme "file")
      (find-file-other-window (embterm--file-uri-path uri)))
     (t (browse-url uri)))))

(defvar embterm--last-selected-time 0 "最後に選択を kill-ring へ入れた時刻。")

(defun embterm--on-selected (text)
  "選択を終えたときの TEXT を（`embterm-copy-on-select' なら）kill-ring へ入れる。
ダブル・トリプルクリックは押すたびに知らせが来るので、すぐ続いて前の内容を含むものは前の項目を置き換える。"
  (when embterm-copy-on-select
    (let ((now (float-time)))
      (kill-new text
                (and kill-ring
                     (< (- now embterm--last-selected-time) 0.6)
                     (string-match-p (regexp-quote (current-kill 0)) text)))
      (setq embterm--last-selected-time now))))

(defun embterm--select-pane-window (id)
  "クリックされたペイン ID のウィンドウを選ぶ（子 HWND はフォーカスを取らないため）。"
  (let* ((pane (embterm--pane-by-id id))
         (win (and pane (embterm--pane-window pane))))
    (when (window-live-p win)
      (unless (eq (window-frame win) (selected-frame))
        (select-frame-set-input-focus (window-frame win)))
      (select-window win))))

(defun embterm--exited (code)
  (setq embterm--exited t)
  (force-mode-line-update)
  (message "embterm: プロセスが終わりました（%s）%s"
           (buffer-name)
           (if code (format "、終了コード %d" code) ""))
  (when embterm-kill-buffer-on-exit
    (kill-buffer (current-buffer))))

;;;; 設定の反映

(defun embterm--font-args ()
  "module に渡すフォントの指定（ファミリのベクタ・大きさ・フォールバックのベクタ）。
大きさには、このバッファの `text-scale-mode' の倍率を掛ける。"
  (let ((families (if (stringp embterm-font-family) (list embterm-font-family) embterm-font-family))
        (amount (if (bound-and-true-p text-scale-mode) (bound-and-true-p text-scale-mode-amount) 0)))
    (list (and families (vconcat families))
          (if (and (numberp amount) (/= amount 0))
              (* (or embterm-font-size embterm--default-font-size) (expt text-scale-mode-step amount))
            embterm-font-size)
          (and embterm-font-fallback-families (vconcat embterm-font-fallback-families)))))

(defun embterm--apply-font ()
  "このバッファの端末にフォントを反映する（`text-scale-mode-hook' からも呼ぶ）。"
  (when embterm--term
    (apply #'embterm-module-set-font embterm--term (embterm--font-args))))

(defun embterm--apply-settings ()
  "フォント・広いグリフの描き方・スクロールバックの行数を、開いている端末に反映する。"
  (dolist (buf (embterm--live-buffers))
    (with-current-buffer buf
      (ignore-errors
        (embterm--apply-font)
        (embterm-module-set-narrow-wide embterm--term (symbol-name embterm-narrow-wide))
        (embterm-module-set-scrollback embterm--term embterm-scrollback-lines)))))

;;;; 色・カーソル・クリップボードの許可（T-034 / T-035 / T-039）

(defconst embterm--ansi-faces
  '(ansi-color-black ansi-color-red ansi-color-green ansi-color-yellow
    ansi-color-blue ansi-color-magenta ansi-color-cyan ansi-color-white
    ansi-color-bright-black ansi-color-bright-red ansi-color-bright-green ansi-color-bright-yellow
    ansi-color-bright-blue ansi-color-bright-magenta ansi-color-bright-cyan ansi-color-bright-white)
  "16 色を取る face（ansi-color.el）。")

(defun embterm--color-hex (color &optional frame)
  "COLOR（色の名前か \"#rgb\"）を \"#rrggbb\" にする。色でなければ nil。"
  (when (and (stringp color) (not (string-prefix-p "unspecified" color)))
    (when-let* ((v (ignore-errors (color-values color frame))))
      (apply #'format "#%02x%02x%02x" (mapcar (lambda (x) (ash x -8)) v)))))

(defun embterm--face-hex (face attribute &optional frame)
  "FACE の ATTRIBUTE（`:foreground' か `:background'。継承を含む）の色を \"#rrggbb\" で。無ければ nil。"
  (when (facep face)
    (embterm--color-hex (face-attribute face attribute frame t) frame)))

(defun embterm--color-args ()
  "`embterm-module-set-colors' の、端末の後ろの引数のリスト。
FG BG CURSOR SELECTION-BG SELECTION-FG PALETTE の順。`embterm-follow-theme' なら
選んでいるフレームの face から、そうでなければ `embterm-color-*' から作る。"
  (if embterm-follow-theme
      (let ((f (selected-frame)))
        (list (embterm--face-hex 'default :foreground f)
              (embterm--face-hex 'default :background f)
              (embterm--face-hex 'cursor :background f)
              (embterm--face-hex 'region :background f)
              (embterm--face-hex 'region :foreground f)
              (vconcat (mapcar (lambda (face) (embterm--face-hex face :foreground f)) embterm--ansi-faces))))
    (list (embterm--color-hex embterm-color-foreground)
          (embterm--color-hex embterm-color-background)
          (embterm--color-hex embterm-color-cursor)
          (embterm--color-hex embterm-color-selection)
          (embterm--color-hex embterm-color-selection-foreground)
          (vconcat (seq-take (mapcar #'embterm--color-hex embterm-color-palette) 16)))))

(defun embterm--apply-term-settings (colors)
  "このバッファの端末に、色 COLORS（`embterm--color-args'）・カーソル・クリップボードの許可を送る（待たない）。"
  (when embterm--term
    (apply #'embterm-module-set-colors embterm--term colors)
    (embterm-module-set-cursor embterm--term (symbol-name embterm-cursor-style) embterm-cursor-blink)
    (embterm-module-set-clipboard-policy embterm--term embterm-allow-clipboard-write embterm-allow-clipboard-read)))

(defun embterm--apply-term-settings-all ()
  "開いているすべての端末に、色・カーソル・クリップボードの許可を送る。"
  (when (and embterm--module-loaded (embterm--live-buffers))
    (let ((colors (embterm--color-args)))
      (dolist (buf (embterm--live-buffers))
        (with-current-buffer buf
          (ignore-errors (embterm--apply-term-settings colors)))))))

(defun embterm-sync-colors ()
  "開いている端末の色・カーソルを今の設定（と face）に合わせ直す。
テーマを替えたときは自動で呼ばれる。`set-face-attribute' で face を直接変えたときに使う。"
  (interactive)
  (embterm--apply-term-settings-all))

(defun embterm--on-theme-change (&rest _)
  "テーマが変わった（`enable-theme-functions' / `disable-theme-functions'）。追従するなら色を送り直す。"
  (when embterm-follow-theme
    (embterm--apply-term-settings-all)))

(add-hook 'enable-theme-functions #'embterm--on-theme-change)
(add-hook 'disable-theme-functions #'embterm--on-theme-change)

;;;; キー（T-009 の案 A）

(defun embterm--mods-string (mods)
  (concat (if (memq 'control mods) "C" "")
          (if (memq 'meta mods) "M" "")
          (if (memq 'shift mods) "S" "")))

(defun embterm--event-key (ev)
  "イベント EV を module に送る (KEY . MODS) にする。送れないものは nil。
KEY は基本のキー（文字かファンクションキーの名前）、MODS は修飾キーの文字列（T-069）。
`event-basic-type' と `event-modifiers' は C-a（1）も `C-S-a' も C-SPC も
`(a (control))' の形に揃える。"
  (when (or (integerp ev) (symbolp ev))
    (let ((base (event-basic-type ev))
          (mods (embterm--mods-string (event-modifiers ev))))
      (cond ((characterp base) (cons (string base) mods))
            ((symbolp base) (cons (symbol-name base) mods))))))

(defun embterm--require-term ()
  (unless embterm--term
    (user-error "embterm: 端末のバッファではありません")))

(defun embterm--send-key (key mods)
  (embterm--require-term)
  (embterm-module-send-key embterm--term key mods))

(defun embterm--send-event ()
  "いま押したキーを端末に送る。"
  (interactive)
  (embterm--require-term)
  (let ((spec (embterm--event-key last-command-event)))
    (if spec
        (embterm-module-send-key embterm--term (car spec) (cdr spec))
      (message "embterm: 送れないキーです: %s" (key-description (this-command-keys-vector))))))

(defun embterm--self-insert ()
  "文字（IME で確定した文字を含む）を端末に送る。"
  (interactive)
  (embterm--require-term)
  (let ((ev last-command-event))
    (if (characterp ev)
        ;; 文字もキーとして送る（win32-input-mode なら 1 文字ずつ Vk = 0 + Uc の形になる）
        (embterm-module-send-key embterm--term (string ev) "")
      (embterm--send-event))))

(defun embterm-send-string (string)
  "STRING をそのまま端末に送る。"
  (interactive "s端末に送る文字列: ")
  (embterm--require-term)
  (embterm-module-send-string embterm--term string))

(defun embterm-send-C-c () "端末に C-c を送る。" (interactive) (embterm--send-key "c" "C"))
(defun embterm-send-C-z () "端末に C-z を送る。" (interactive) (embterm--send-key "z" "C"))
(defun embterm-send-C-d () "端末に C-d を送る。" (interactive) (embterm--send-key "d" "C"))
(defun embterm-send-C-backslash () "端末に C-\\ を送る。" (interactive) (embterm--send-key "\\" "C"))

(defun embterm-reset-keyboard ()
  "端末のキーの送り方（Kitty keyboard protocol のフラグ）を今の画面の分だけ戻す（T-090）。
KKP を求めたアプリが戻さずに終わり、そのアプリの子孫が残っているなどで自動の安全策が効かないと、
シェルに `[27;5u' のようなゴミ文字が入る。そのときに使う。"
  (interactive)
  (embterm--require-term)
  (message (if (embterm-module-reset-keyboard embterm--term)
               "embterm: キーの送り方を戻しました"
             "embterm: 戻すものはありませんでした")))

(defun embterm-yank ()
  "kill-ring の先頭を端末に貼り付ける（ブラケットペーストのモードなら包む）。"
  (interactive)
  (embterm--require-term)
  (let ((text (current-kill 0)))
    (when (or (embterm-module-paste-safe-p embterm--term text)
              (not embterm-confirm-unsafe-paste)
              (y-or-n-p "embterm: 改行を含むテキストを貼り付けますか？ "))
      (embterm-module-paste embterm--term text))))

(defun embterm-copy-or-send ()
  "端末の選択があれば kill-ring へ入れて選択を消す。無ければ `M-w' を端末に送る。"
  (interactive)
  (embterm--require-term)
  (let ((text (embterm-module-selection embterm--term)))
    (if text
        (progn
          (kill-new text)
          (embterm-module-clear-selection embterm--term)
          (message "embterm: 選択をコピーしました（%d 文字）" (length text)))
      (embterm--send-event))))

(defun embterm-wheel (event)
  "ホイールを Emacs が受けたとき（`MouseWheelRouting' の設定による）、そのウィンドウのペインを動かす。
マウスの報告が有効なアプリならアプリへ送る（module が判断する）。"
  (interactive "e")
  (let* ((win (posn-window (event-start event)))
         (buf (if (windowp win) (window-buffer win) (current-buffer))))
    (when (buffer-local-value 'embterm--term buf)
      (with-current-buffer buf
        (let ((pane (or (embterm--pane-of win) (car embterm--panes)))
              (notches (pcase (event-basic-type event)
                         ((or 'wheel-up 'mouse-4) 1)
                         ((or 'wheel-down 'mouse-5) -1))))
          (when (and pane notches)
            (embterm-module-scroll embterm--term (embterm--pane-id pane) "wheel" notches)))))))

(defun embterm--scroll-selected (kind &optional n)
  "選んでいるウィンドウのペインをスクロールする。KIND と N は `embterm-module-scroll' と同じ。"
  (embterm--require-term)
  (let ((pane (embterm--pane-of (selected-window))))
    (when pane
      (embterm-module-scroll embterm--term (embterm--pane-id pane) kind n))))

(defun embterm-scroll-page-up ()
  "このウィンドウのペインを 1 ページ遡る（スクロールバック）。"
  (interactive)
  (embterm--scroll-selected "pages" -1))

(defun embterm-scroll-page-down ()
  "このウィンドウのペインを 1 ページ戻す。"
  (interactive)
  (embterm--scroll-selected "pages" 1))

(defun embterm-scroll-top ()
  "このウィンドウのペインをスクロールバックの先頭へ。"
  (interactive)
  (embterm--scroll-selected "top"))

(defun embterm-scroll-bottom ()
  "このウィンドウのペインをいちばん下へ戻す。"
  (interactive)
  (embterm--scroll-selected "bottom"))

(defun embterm-screenshot (file)
  "端末の今の画面を PNG の FILE に保存する（デバッグ用。待たない）。"
  (interactive "F保存する PNG: ")
  (embterm--require-term)
  (embterm-module-screenshot embterm--term (expand-file-name file) nil))

(defun embterm-screen-text ()
  "端末の今の画面のテキスト（デバッグ用）。"
  (embterm--require-term)
  (embterm-module-text embterm--term))

(defvar embterm-semi-char-map (make-sparse-keymap)
  "semi-char モードのキーマップ。`embterm-keymap-exceptions' 以外のキーを端末に送る。
`embterm-rebuild-keymap' が中身を作る。")

(defun embterm-rebuild-keymap ()
  "`embterm-semi-char-map' を `embterm-keymap-exceptions' から作り直す。
並びは ghostel の semi-char と同じ。"
  (interactive)
  (let ((map (make-sparse-keymap))
        (bind (lambda (map key)
                (unless (member key embterm-keymap-exceptions)
                  (ignore-errors (define-key map (kbd key) #'embterm--send-event))))))
    (define-key map [remap self-insert-command] #'embterm--self-insert)
    ;; ファンクションキーと修飾キーの組
    (dolist (key (append
                  '("<return>" "<tab>" "<backspace>" "<escape>" "<delete>" "<deletechar>"
                    "<up>" "<down>" "<right>" "<left>" "<home>" "<end>" "<prior>" "<next>" "<insert>"
                    "<print>" "<pause>" "<scroll-lock>" "<apps>" "<menu>"
                    "<kp-enter>" "<kp-tab>" "<kp-add>" "<kp-subtract>" "<kp-multiply>" "<kp-divide>"
                    "<kp-decimal>" "<kp-separator>" "<kp-home>" "<kp-end>" "<kp-prior>" "<kp-next>"
                    "<kp-up>" "<kp-down>" "<kp-left>" "<kp-right>" "<kp-insert>" "<kp-delete>")
                  (mapcar (lambda (n) (format "<f%d>" n)) (number-sequence 1 24))
                  (mapcar (lambda (n) (format "<kp-%d>" n)) (number-sequence 0 9))))
      (dolist (mod '("" "S-" "C-" "M-" "C-S-" "M-S-" "C-M-" "C-M-S-"))
        (funcall bind map (concat mod key))))
    ;; 修飾キーが付いた SPC（`C-SPC' は PSReadLine の補完メニューなど）と、ふつうの文字にならない組
    (dolist (key '("RET" "TAB" "DEL" "<backtab>" "C-@" "M-DEL" "M-SPC" "C-SPC" "S-SPC" "C-S-SPC" "C-M-SPC" "M-S-SPC"
                   "C-M-S-SPC" "C-]" "C-/" "C-M-]" "C-M-/"))
      (funcall bind map key))
    ;; スクロールバックは Emacs に残す（T-037）。送るキーの一覧で束縛したあとで上書きする
    (define-key map (kbd "S-<prior>") #'embterm-scroll-page-up)
    (define-key map (kbd "S-<next>") #'embterm-scroll-page-down)
    (define-key map (kbd "S-<home>") #'embterm-scroll-top)
    (define-key map (kbd "S-<end>") #'embterm-scroll-bottom)
    ;; C-<英字>（C-i = TAB、C-m = RET は上。C-y は貼り付け）、C-M-<英字>、C-S-<英字>（`C-S-a' は C-a と別のキーとして送れる）
    (dolist (c (number-sequence ?a ?z))
      (unless (memq c '(?i ?m ?y))
        (funcall bind map (format "C-%c" c)))
      (funcall bind map (format "C-S-%c" c))
      (funcall bind map (format "C-M-%c" c))
      (funcall bind map (format "C-M-S-%c" c)))
    ;; M-<ASCII の印字可能な文字>、C-M-<ASCII の印字可能な文字>
    (dolist (c (number-sequence ?! ?~))
      (funcall bind map (format "M-%c" c))
      (funcall bind map (format "C-M-%c" c)))
    ;; C-<数字・記号>（C-[ は ESC の接頭辞を兼ねるので束縛しない。C-i などの英字は上）
    (dolist (c (string-to-list "0123456789-=]\\;',./`!\"#$%&()*+:<>?@^_{|}~"))
      (funcall bind map (format "C-%c" c)))
    ;; M-<非 ASCII の文字>（`M-あ'。T-089）。Emacs は M-あ を ESC あ としても引くので、ESC の下の表に文字の範囲で束縛する。
    ;; アプリには ESC + 文字（レガシー）か Alt 付きのキー（win32-input-mode / KKP）で届く。
    ;; C-あ は送らない（修飾キー付きの文字は範囲で束縛できず、既定の束縛 [t] では C-x などの接頭キーまで奪うため）
    (let ((esc (lookup-key map [27])))
      (when (keymapp esc)
        (define-key esc (vector (cons #x80 (max-char))) #'embterm--send-event)))
    ;; 選択があれば M-w はコピー（T-036）
    (unless (member "M-w" embterm-keymap-exceptions)
      (define-key map (kbd "M-w") #'embterm-copy-or-send))
    ;; 貼り付けとホイール
    (unless (member "C-y" embterm-keymap-exceptions)
      (define-key map (kbd "C-y") #'embterm-yank))
    (define-key map (kbd "S-<insert>") #'embterm-yank)
    (define-key map [remap yank] #'embterm-yank)
    (define-key map [remap mwheel-scroll] #'embterm-wheel)
    (define-key map [remap pixel-scroll-precision] #'embterm-wheel)
    (setcdr embterm-semi-char-map (cdr map))))

(embterm-rebuild-keymap)

(defvar embterm--emulation-alist `((embterm--semi-char . ,embterm-semi-char-map))
  "`embterm-semi-char-map' を、マイナーモードより優先される層に置くためのもの。
`cua-mode' などは `emulation-mode-map-alists' にキーマップを置くので、その先頭に置かないと
`C-v' などを取られる（GenPreview f8a509f と同じ理由）。")

(defun embterm--emulation-first ()
  "`embterm--emulation-alist' を `emulation-mode-map-alists' の先頭に置く。
あとから読み込まれたパッケージが前に入ることがあるので、端末を開くたびとポーリングのたびに置き直す。"
  (setq emulation-mode-map-alists
        (cons 'embterm--emulation-alist
              (delq 'embterm--emulation-alist emulation-mode-map-alists))))

;;;; 入力モード

(defun embterm-semi-char-mode ()
  "semi-char モードにする（`embterm-keymap-exceptions' 以外のキーを端末に送る）。"
  (interactive)
  (embterm--require-term)
  (setq embterm--input-mode 'semi-char
        embterm--semi-char t
        embterm--copy-window nil
        cursor-type nil)
  (embterm--placeholder)
  (force-mode-line-update)
  (embterm--sync))

(defun embterm-copy-mode ()
  "copy モードにする。端末を隠し、スクロールバックごとのテキストをバッファに写して、キーを Emacs に返す。
\\<embterm-mode-map>\\[embterm-semi-char-mode] で戻る（ペインのスクロール位置はそのまま）。
テキストは module が裏で作り、できたら（`embterm--fill-copy'）バッファに入る。"
  (interactive)
  (embterm--require-term)
  (let ((pane (embterm--pane-of (selected-window)))
        (inhibit-read-only t))
    (setq embterm--input-mode 'copy
          embterm--semi-char nil
          embterm--copy-filling nil
          embterm--copy-window (selected-window)
          cursor-type t)
    (erase-buffer)
    (insert (propertize "EmbTerm: スクロールバックを写しています…\n" 'face 'shadow))
    (goto-char (point-min))
    (embterm-module-request-copy embterm--term (and pane (embterm--pane-id pane))))
  (force-mode-line-update)
  (embterm--sync)
  (message "embterm: copy モード（%s で戻る）"
           (substitute-command-keys "\\<embterm-mode-map>\\[embterm-semi-char-mode]")))

(defun embterm--fill-copy ()
  "module が作った copy モードのテキストの次の一部をバッファに足す（T-086）。
テキストは 1 MiB ずつ届くので、残りがあれば次の一部をタイマーで受け取る（大きなスクロールバックでも Emacs を長く止めない）。
最後まで受け取ったら、端末が折り返した行の改行に印（`embterm-wrap'）を付け、ポイントを見ていた位置
（ペインのスクロール位置 / カーソルの行）にする。copy モードを抜けていたら残りを捨てる。"
  (pcase (embterm-module-take-copy embterm--term)
    (`(,text ,top ,point ,wraps ,more)
     (if (not (eq embterm--input-mode 'copy))
         (progn
           (setq embterm--copy-filling nil)
           (while (and more (setq more (car (last (embterm-module-take-copy embterm--term)))))))
       (let ((inhibit-read-only t))
         (unless embterm--copy-filling
           (erase-buffer)
           (setq embterm--copy-filling (list top point wraps)))
         (save-excursion
           (goto-char (point-max))
           (insert text)))
       (if more
           (let ((buf (current-buffer)))
             (run-at-time 0 nil (lambda ()
                                  (when (buffer-live-p buf)
                                    (with-current-buffer buf
                                      (when embterm--term (embterm--fill-copy)))))))
         (pcase-let ((`(,top ,point ,wraps) embterm--copy-filling))
           (setq embterm--copy-filling nil)
           (embterm--mark-wraps wraps)
           (let* ((window embterm--copy-window)
                  (goto (lambda (line)
                          (goto-char (point-min))
                          (forward-line line)
                          (point)))
                  (top-pos (funcall goto top))
                  (point-pos (funcall goto point)))
             (goto-char point-pos)
             (when (and (window-live-p window) (eq (window-buffer window) (current-buffer)))
               (set-window-start window top-pos t)
               (set-window-point window point-pos)))))))))

(defun embterm--mark-wraps (wraps)
  "WRAPS（端末が折り返した行の番号のベクタ。昇順）の行末の改行に `embterm-wrap' を付ける。
バッファの行は端末の行のまま（行番号とスクロール位置が対応する）にし、コピーのときだけつなぐ
（`embterm--filter-buffer-substring'）。"
  (let ((inhibit-read-only t)
        (line 0))
    (save-excursion
      (goto-char (point-min))
      (seq-doseq (w wraps)
        (forward-line (- w line))
        (setq line w)
        (end-of-line)
        (when (eq (char-after) ?\n)
          (put-text-property (point) (1+ (point)) 'embterm-wrap t))))))

(defun embterm--filter-buffer-substring (beg end &optional delete)
  "`filter-buffer-substring-function' の端末のバッファ版。端末が折り返した行の改行（`embterm-wrap'）を除く
（copy モードで `M-w' などしたとき、折り返しをつないだ形でコピーする。T-086）。"
  (let ((s (buffer-substring--filter beg end delete))
        (pos 0) parts next)
    (while (setq next (text-property-any pos (length s) 'embterm-wrap t s))
      (push (substring s pos next) parts)
      (setq pos (1+ next)))
    (if parts
        (apply #'concat (nreverse (cons (substring s pos) parts)))
      s)))

(provide 'embterm)

;;; embterm.el ends here
