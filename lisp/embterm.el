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
;;   `window-selection-change-functions' で、端末のバッファを出しているウィンドウの
;;   `window-inside-pixel-edges' を module に送る。見えなくなったら隠す。同じバッファを複数の
;;   ウィンドウに出したときは、子 HWND は 1 枚だけ（選んでいるウィンドウ → 前に出していたウィンドウ →
;;   最初のウィンドウ。GenPreview と同じ。本格的な split は T-063）
;; - キー（T-009 の案 A）: フォーカスは Emacs に残る。semi-char モードでは
;;   `embterm-keymap-exceptions' 以外のキーを module に送る（ghostel の semi-char と同じ）。
;;   キーマップは `emulation-mode-map-alists' の先頭に置く（cua-mode などに負けないため）
;;   `C-c C-k' で copy モード（キーを Emacs に返し、画面のテキストをバッファに写す）、`C-c C-j' で戻る
;; - module → Lisp はタイマー（`embterm-poll-interval'）でポーリングする（タイトル・カレントディレクトリ・
;;   クリック・プロセスの終了）。module の関数は待たない
;; - module は一時ディレクトリにコピーしてから読む（読み込み済みの DLL は差し替えられないため。T-030）。
;;   新しい版を使うには Emacs の再起動が要る

;;; Code:

(require 'cl-lib)
(require 'seq)
(require 'subr-x)
(require 'url-util)

(declare-function embterm-module-version "embterm-module")
(declare-function embterm-module-set-log "embterm-module")
(declare-function embterm-module-create "embterm-module")
(declare-function embterm-module-place "embterm-module")
(declare-function embterm-module-hide "embterm-module")
(declare-function embterm-module-send-string "embterm-module")
(declare-function embterm-module-send-key "embterm-module")
(declare-function embterm-module-paste "embterm-module")
(declare-function embterm-module-scroll "embterm-module")
(declare-function embterm-module-set-font "embterm-module")
(declare-function embterm-module-set-narrow-wide "embterm-module")
(declare-function embterm-module-focus "embterm-module")
(declare-function embterm-module-screenshot "embterm-module")
(declare-function embterm-module-close "embterm-module")
(declare-function embterm-module-poll "embterm-module")
(declare-function embterm-module-cursor "embterm-module")
(declare-function embterm-module-text "embterm-module")

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
  "端末のフォント。nil なら module の既定（HackGen Console NF → Cascadia Mono → Consolas）。"
  :type '(choice (const :tag "既定" nil) string)
  :set #'embterm--set-and-apply)

(defcustom embterm-font-size nil
  "端末のフォントの大きさ（ポイント）。nil なら module の既定（12）。"
  :type '(choice (const :tag "既定" nil) number)
  :set #'embterm--set-and-apply)

(defcustom embterm-narrow-wide 'overlap
  "幅 1 と数えたのにグリフが広い文字（`①' `★' `※' など）の描き方（T-013）。
`overlap' は縮めずに右隣へはみ出させる（WezTerm と同じ見え方）、`shrink' は 1 セルに縮める（ghostel と同じ）。"
  :type '(choice (const overlap) (const shrink))
  :set #'embterm--set-and-apply)

(defcustom embterm-kill-buffer-on-exit t
  "non-nil なら、プロセスが終わったら端末のバッファを消す。"
  :type 'boolean)

(defcustom embterm-buffer-name-format "*embterm: %s*"
  "タイトル（OSC 0 / 2）をバッファ名にするときの書式。nil ならバッファ名を変えない。"
  :type '(choice (const :tag "変えない" nil) string))

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
  "DIR を `\\' 区切りの絶対パスにする（Windows の API に渡すため）。"
  (subst-char-in-string ?/ ?\\ (directory-file-name (expand-file-name dir))))

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

(defvar-local embterm--term nil "module の端末（user-ptr）。")
(defvar-local embterm--window nil "子 HWND を出しているウィンドウ。")
(defvar-local embterm--placed nil "最後に置いた (WINDOW-ID WINDOW EDGES)。同じなら置き直さない。")
(defvar-local embterm--view-seen nil "子 HWND があるのを見たか（消えたら置き直すため）。")
(defvar-local embterm--generation nil "最後に読んだ module の状態の世代。")
(defvar-local embterm--state nil "最後に読んだ module の状態（plist）。")
(defvar-local embterm--title nil "最後に反映したタイトル。")
(defvar-local embterm--focused 'unknown "最後に module に伝えたフォーカスの状態。")
(defvar-local embterm--exited nil "プロセスが終わったか。")
(defvar-local embterm--input-mode 'semi-char
  "入力モード。`semi-char'（キーを端末に送る）か `copy'（キーを Emacs に返し、画面のテキストを写す）。")
(defvar-local embterm--semi-char nil
  "non-nil なら `embterm-semi-char-map' が効く（`emulation-mode-map-alists' の条件）。")

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

;;;###autoload
(defun embterm ()
  "新しい端末のバッファを開く（`embterm-shell' を `default-directory' で起動する）。"
  (interactive)
  (embterm--load-module)
  (let* ((dir default-directory)
         (argv (embterm--argv))
         (buf (generate-new-buffer "*embterm*")))
    (with-current-buffer buf
      (setq default-directory dir)
      (embterm-mode)
      (condition-case err
          (setq embterm--term
                (embterm-module-create (vconcat argv) (embterm--cwd) (embterm--conpty-dir)
                                       embterm-font-family embterm-font-size
                                       (symbol-name embterm-narrow-wide)))
        (error (kill-buffer buf)
               (signal (car err) (cdr err))))
      (push buf embterm--buffers))
    (pop-to-buffer-same-window buf)
    (embterm--activate)
    (embterm--sync)
    buf))

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
  (embterm--placeholder)
  (embterm--emulation-first)
  (add-hook 'kill-buffer-hook #'embterm--close nil t))

(defun embterm--mode-line ()
  (concat (if (eq embterm--input-mode 'copy) " [copy]" "")
          (when embterm--exited " [終了]")))

;;;; 閉じる

(defun embterm--close ()
  "このバッファの端末を閉じる（待たない。PTY の後始末は module が裏で行う）。"
  (when embterm--term
    (ignore-errors (embterm-module-close embterm--term))
    (setq embterm--term nil))
  (setq embterm--buffers (delq (current-buffer) embterm--buffers)))

(defun embterm--close-all ()
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

(defun embterm--pick-window ()
  "子 HWND を出すウィンドウ。選んでいるウィンドウ → 前に出していたウィンドウ → 最初のウィンドウ。"
  (let ((wins (get-buffer-window-list (current-buffer) nil 'visible)))
    (cond ((memq (selected-window) wins) (selected-window))
          ((memq embterm--window wins) embterm--window)
          (t (car wins)))))

(defun embterm--place ()
  "このバッファの端末を置く（出しているウィンドウが無い・copy モードなら隠す）。"
  (let ((win (and (eq embterm--input-mode 'semi-char) (embterm--pick-window))))
    (if (not win)
        (when embterm--placed
          (embterm-module-hide embterm--term)
          (setq embterm--placed nil))
      (let* ((frame (window-frame win))
             (id (frame-parameter frame 'window-id))
             (edges (window-inside-pixel-edges win))
             (key (list id win edges)))
        (setq embterm--window win)
        (unless (equal key embterm--placed)
          (pcase-let ((`(,l ,tp ,r ,b) edges))
            (embterm-module-place embterm--term id l tp (- r l) (- b tp)))
          (setq embterm--placed key))))))

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
  (unless embterm--buffers
    (embterm--deactivate)))

(defun embterm--poll-1 ()
  ;; フックが拾わない位置の変化（あれば）もここで拾う。同じなら module には送らない
  (embterm--place)
  (let ((st (embterm-module-poll embterm--term embterm--generation)))
    (when st
      (setq embterm--generation (plist-get st :generation)
            embterm--state st)
      ;; 親のフレームが消されると子 HWND も消える（端末は残る）。置き直せば作り直される
      (if (plist-get st :view)
          (setq embterm--view-seen t)
        (when (and embterm--view-seen (plist-get st :alive))
          (setq embterm--view-seen nil
                embterm--placed nil)
          (embterm--place)))
      (embterm--apply-title (plist-get st :title))
      (embterm--apply-pwd (plist-get st :pwd))
      (dolist (e (plist-get st :events))
        (pcase e
          (`(clicked) (embterm--select-window))
          (`(bell) (ding t))
          (`(exited . ,code) (embterm--exited code))
          (`(screenshot . ,path) (message "embterm: 画面を保存しました: %s" path))
          (`(error . ,m) (message "embterm: %s" m))
          (_ nil))))))

(defun embterm--apply-title (title)
  (when (and embterm-buffer-name-format title (not (string-empty-p title))
             (not (equal title embterm--title)))
    (setq embterm--title title)
    (rename-buffer (format embterm-buffer-name-format title) t)))

(defun embterm--apply-pwd (pwd)
  "カレントディレクトリ（OSC 7 の `file://HOST/PATH' か、パス）を `default-directory' にする。"
  (when (and pwd (not (string-empty-p pwd)))
    (let ((path (if (string-match "\\`file://[^/]*\\(/.*\\)\\'" pwd)
                    (let ((p (url-unhex-string (match-string 1 pwd))))
                      ;; /C:/Users → C:/Users
                      (if (string-match-p "\\`/[A-Za-z]:" p) (substring p 1) p))
                  pwd)))
      (when (file-directory-p path)
        (setq default-directory (file-name-as-directory (expand-file-name path)))))))

(defun embterm--select-window ()
  "クリックされた端末のウィンドウを選ぶ（子 HWND はフォーカスを取らないため）。"
  (let ((win embterm--window))
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

(defun embterm--apply-settings ()
  "フォントと広いグリフの描き方を、開いている端末に反映する。"
  (dolist (buf (embterm--live-buffers))
    (with-current-buffer buf
      (ignore-errors
        (embterm-module-set-font embterm--term embterm-font-family embterm-font-size)
        (embterm-module-set-narrow-wide embterm--term (symbol-name embterm-narrow-wide))))))

;;;; キー（T-009 の案 A）

(defun embterm--mods-string (mods)
  (concat (if (memq 'control mods) "C" "")
          (if (memq 'meta mods) "M" "")
          (if (memq 'shift mods) "S" "")))

(defun embterm--event-key (ev)
  "イベント EV を module に送る (KEY . MODS) にする。送れないものは nil。"
  (cond
   ((integerp ev)
    (let ((meta (/= 0 (logand ev ?\M-\0)))
          (c (logand ev (lognot ?\M-\0))))
      (if (characterp c)
          ;; 文字（C-a のような制御文字を含む）。Meta だけ外して渡す
          (cons (string c) (if meta "M" ""))
        ;; C-% や C-SPC のように、文字にならない修飾キーの組
        (let ((base (event-basic-type ev)))
          (when (characterp base)
            (cons (string base) (embterm--mods-string (event-modifiers ev))))))))
   ((symbolp ev)
    (let ((base (event-basic-type ev)))
      (when (symbolp base)
        (cons (symbol-name base) (embterm--mods-string (event-modifiers ev))))))))

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
        (embterm-module-send-string embterm--term (string ev))
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

(defun embterm-yank ()
  "kill-ring の先頭を端末に貼り付ける（ブラケットペーストのモードなら包む）。"
  (interactive)
  (embterm--require-term)
  (embterm-module-paste embterm--term (current-kill 0)))

(defun embterm-wheel (event)
  "ホイールを Emacs が受けたとき（`MouseWheelRouting' の設定による）、端末のスクロールバックを動かす。"
  (interactive "e")
  (embterm--require-term)
  (pcase (event-basic-type event)
    ((or 'wheel-down 'mouse-5) (embterm-module-scroll embterm--term 3))
    ((or 'wheel-up 'mouse-4) (embterm-module-scroll embterm--term -3))))

(defun embterm-screenshot (file)
  "端末の今の画面を PNG の FILE に保存する（デバッグ用。待たない）。"
  (interactive "F保存する PNG: ")
  (embterm--require-term)
  (embterm-module-screenshot embterm--term (expand-file-name file)))

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
    (dolist (key '("<return>" "<tab>" "<backspace>" "<escape>" "<delete>" "<deletechar>"
                   "<up>" "<down>" "<right>" "<left>" "<home>" "<end>" "<prior>" "<next>" "<insert>"
                   "<f1>" "<f2>" "<f3>" "<f4>" "<f5>" "<f6>" "<f7>" "<f8>" "<f9>" "<f10>" "<f11>" "<f12>"))
      (dolist (mod '("" "S-" "C-" "M-" "C-S-" "M-S-" "C-M-"))
        (funcall bind map (concat mod key))))
    (dolist (key '("RET" "TAB" "DEL" "<backtab>" "C-@" "M-DEL" "M-SPC" "C-]" "C-/" "C-M-]" "C-M-/"))
      (funcall bind map key))
    ;; C-<英字>（C-i = TAB、C-m = RET は上。C-y は貼り付け）と C-M-<英字>
    (dolist (c (number-sequence ?a ?z))
      (unless (memq c '(?i ?m ?y))
        (funcall bind map (format "C-%c" c)))
      (funcall bind map (format "C-M-%c" c)))
    ;; M-<ASCII の印字可能な文字>
    (dolist (c (number-sequence ?! ?~))
      (funcall bind map (format "M-%c" c)))
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
        cursor-type nil)
  (embterm--placeholder)
  (force-mode-line-update)
  (embterm--sync))

(defun embterm-copy-mode ()
  "copy モードにする。端末を隠し、今の画面のテキストをバッファに写して、キーを Emacs に返す。
\\<embterm-mode-map>\\[embterm-semi-char-mode] で戻る。"
  (interactive)
  (embterm--require-term)
  (let ((text (or (embterm-module-text embterm--term) "")))
    (setq embterm--input-mode 'copy
          embterm--semi-char nil
          cursor-type t)
    (let ((inhibit-read-only t))
      (erase-buffer)
      (insert text)
      (goto-char (point-max))
      (skip-chars-backward " \n")))
  (force-mode-line-update)
  (embterm--sync)
  (message "embterm: copy モード（%s で戻る）"
           (substitute-command-keys "\\<embterm-mode-map>\\[embterm-semi-char-mode]")))

(provide 'embterm)

;;; embterm.el ends here
