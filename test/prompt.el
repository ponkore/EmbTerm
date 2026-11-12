;;; prompt.el --- プロンプトの境界（OSC 133。T-119）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/prompt.el
;;
;; target/dist/etc/embterm.{ps1,bash,lua} を読み込んだ pwsh・Git Bash・nyagos で、OSC 133 の印を使う機能を確かめる。
;; 結果は target/t119/prompt.log（OK / NG）。`sit-for' は使わない
;; 確かめること:
;;   - C-c C-p / C-c C-n（`embterm-previous-prompt' / `embterm-next-prompt'）で前後のプロンプトをペインの一番上にする
;;   - C-c C-o（`embterm-copy-last-output'）で直前のコマンドの出力だけを kill-ring へ
;;   - 0 でない終了コードをモードラインに出し、`embterm-command-end-functions' を呼ぶ

(require 'cl-lib)
(require 'seq)

(defconst pr-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst pr-out (expand-file-name "target/t119/" pr-root))
(defconst pr-log-file (expand-file-name "prompt.log" pr-out))
(defconst pr-etc (expand-file-name "target/dist/etc/" pr-root))
(defconst pr-git-bash "C:/Program Files/Git/bin/bash.exe")

(make-directory pr-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p pr-log-file) (delete-file pr-log-file))

(defvar pr-ng 0)
(defvar pr-buf nil)
(defvar pr-ends nil "`embterm-command-end-functions' が受けた終了コード。")

(defun pr-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil pr-log-file t 'silent))

(defun pr-check (name ok &optional detail)
  (unless ok
    (cl-incf pr-ng)
    (when (buffer-live-p pr-buf)
      (pr-log "画面:\n%s" (with-current-buffer pr-buf (embterm-screen-text)))))
  (pr-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun pr-send (s) (with-current-buffer pr-buf (embterm-send-string s)))

(defun pr-top-line ()
  "選んでいるウィンドウのペインの一番上の行。"
  (with-current-buffer pr-buf
    (let ((pane (embterm--pane-of (get-buffer-window pr-buf))))
      (car (split-string (or (embterm-module-pane-text embterm--term (embterm--pane-id pane)) "") "\n")))))

(defun pr-in-term (fn)
  (with-selected-window (get-buffer-window pr-buf)
    (with-current-buffer pr-buf (call-interactively fn))))

(defun pr-last-output ()
  (with-current-buffer pr-buf (embterm-module-last-output embterm--term)))

(add-to-list 'load-path (expand-file-name "target/dist" pr-root))
(require 'embterm)
(set-frame-size nil 110 30)
(add-hook 'embterm-command-end-functions (lambda (code _buf) (push code pr-ends)))

(defun pr-native (f) (subst-char-in-string ?/ ?\\ f))

(defun pr-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (pr-check "エラーなし" nil (format "%S" err))))
                   (pr-run (cdr steps))))))

(pr-run
 `((0.5 . ,(lambda ()
             (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
               (setq pr-buf (embterm-new)))
             (delete-other-windows)))
   (3.5 . ,(lambda ()
             (pr-send (format ". '%s'\r" (pr-native (concat pr-etc "embterm.ps1")))))) ; この行のプロンプトは印が無い
   (1.0 . ,(lambda ()
             (pr-send "1..40 | % { \"a$_\" }\r")))
   (1.5 . ,(lambda ()
             (pr-send "1..40 | % { \"b$_\" }\r")))
   (1.5 . ,(lambda ()
             (pr-send "Write-Output 'last-out-1' 'last-out-2'\r")))
   (1.5 . ,(lambda ()
             (pr-check "pwsh: 直前の出力" (equal (pr-last-output) "last-out-1\nlast-out-2") (format "%S" (pr-last-output)))
             (pr-in-term #'embterm-copy-last-output)
             (pr-check "C-c C-o で kill-ring へ" (equal (car kill-ring) "last-out-1\nlast-out-2") (format "%S" (car kill-ring)))
             (pr-in-term #'embterm-previous-prompt)))
   (0.5 . ,(lambda ()
             ;; 起点はペインの一番上の行。最後の 1 画面の中のプロンプト（last-out）は一番上にできないので、画面の上の外の b へ
             (pr-check "C-c C-p: 画面の上の外の 1 つ前のプロンプト（b）が一番上" (string-match-p "PS .*> 1\\.\\.40 .*b\\$_" (pr-top-line)) (pr-top-line))
             (pr-in-term #'embterm-previous-prompt)))
   (0.5 . ,(lambda ()
             (pr-check "C-c C-p 2 回目: その前のプロンプト（a）" (string-match-p "PS .*> 1\\.\\.40 .*a\\$_" (pr-top-line)) (pr-top-line))
             (pr-in-term #'embterm-next-prompt)))
   (0.5 . ,(lambda ()
             (pr-check "C-c C-n: 次のプロンプト（b）" (string-match-p "PS .*> 1\\.\\.40 .*b\\$_" (pr-top-line)) (pr-top-line))
             (pr-in-term #'embterm-next-prompt)))
   (0.5 . ,(lambda ()
             (let ((p (assq (embterm--pane-id (with-current-buffer pr-buf (embterm--pane-of (get-buffer-window pr-buf))))
                            (plist-get (buffer-local-value 'embterm--state pr-buf) :panes))))
               (pr-check "C-c C-n で最後の画面に入るといちばん下へ戻る" (not (nth 4 p)) (format "%S" p)))
             (setq pr-ends nil)
             (pr-send "cmd /c exit 3\r")))
   (1.5 . ,(lambda ()
             (pr-check "終了コード 3 が届く" (equal (car pr-ends) 3) (format "%S" pr-ends))
             (pr-check "モードラインに [exit 3]" (string-search "[exit 3]" (with-current-buffer pr-buf (embterm--mode-line))))
             (pr-check "出力の無いコマンドの直前の出力は空" (equal (pr-last-output) "") (format "%S" (pr-last-output)))
             (pr-send "'ok'\r")))
   (1.5 . ,(lambda ()
             (pr-check "成功したらモードラインから消える" (not (string-search "[exit" (with-current-buffer pr-buf (embterm--mode-line)))))
             (kill-buffer pr-buf)
             (let ((embterm-shell (list pr-git-bash "--noprofile" "--norc" "-i"))
                   (process-environment (cons "LANG=C.UTF-8" process-environment)))
               (setq pr-buf (embterm-new)))))
   (3.0 . ,(lambda ()
             (pr-send (format ". '%s'\r" (concat pr-etc "embterm.bash")))
             (pr-send "printf 'b1\\nb2\\n'\r")))
   (1.5 . ,(lambda ()
             (pr-check "Git Bash: 直前の出力" (equal (pr-last-output) "b1\nb2") (format "%S" (pr-last-output)))
             (setq pr-ends nil)
             (pr-send "false\r")))
   (1.5 . ,(lambda ()
             (pr-check "Git Bash: 終了コード 1" (equal (car pr-ends) 1) (format "%S" pr-ends))
             (kill-buffer pr-buf)
             (let ((embterm-shell '("nyagos")))
               (setq pr-buf (embterm-new)))))
   (3.0 . ,(lambda ()
             (pr-send (format "lua_e \"dofile([[%s]])\"\r" (pr-native (concat pr-etc "embterm.lua"))))))
   (1.5 . ,(lambda ()
             (pr-send "echo n1\r")))
   (1.5 . ,(lambda ()
             (pr-check "nyagos: 直前の出力" (equal (pr-last-output) "n1") (format "%S" (pr-last-output)))
             (pr-log "失敗の数: %d" pr-ng)
             (kill-emacs (if (zerop pr-ng) 0 1))))))

(run-at-time 60 nil (lambda () (pr-log "時間切れ") (kill-emacs 2)))

;;; prompt.el ends here
