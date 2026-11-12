;;; shellcmd.el --- シェルから Emacs の関数を呼ぶ（OSC 51;E。T-118）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/shellcmd.el
;;
;; target/dist/etc/embterm.{ps1,bash,lua} を pwsh・Git Bash・nyagos で読み込み、関数を呼ぶ。
;; 結果は target/t118/shellcmd.log（OK / NG）。`sit-for' は使わない
;; 確かめること:
;;   - pwsh の embterm_find_file で、日本語と空白を含むパス（カレントディレクトリからの相対）のファイルが開く
;;   - embterm_cmd の引数の " と \ がそのまま届く
;;   - 許可していない関数は呼ばれない。長すぎて切れたものは捨てる。embterm-allow-shell-commands が nil なら呼ばれない
;;   - Git Bash の embterm_dired（cygpath でパスを直す）、nyagos の embterm_cmd

(require 'cl-lib)
(require 'seq)

(defconst sc-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst sc-out (expand-file-name "target/t118/" sc-root))
(defconst sc-log-file (expand-file-name "shellcmd.log" sc-out))
(defconst sc-dir (expand-file-name "日本語 dir/" sc-out))
(defconst sc-etc (expand-file-name "target/dist/etc/" sc-root))
(defconst sc-git-bash "C:/Program Files/Git/bin/bash.exe")

(make-directory sc-dir t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p sc-log-file) (delete-file sc-log-file))
(write-region "hello\n" nil (expand-file-name "あ b.txt" sc-dir) nil 'silent)
(write-region "keep\n" nil (expand-file-name "keep.txt" sc-out) nil 'silent)

(defvar sc-ng 0)
(defvar sc-calls nil "record で呼ばれた引数のリスト。")
(defvar sc-buf nil)

(defun sc-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil sc-log-file t 'silent))

(defun sc-check (name ok &optional detail)
  (unless ok
    (cl-incf sc-ng)
    (when (buffer-live-p sc-buf)
      (sc-log "画面:\n%s" (with-current-buffer sc-buf (embterm-screen-text)))))
  (sc-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun sc-record (&rest args) (push args sc-calls))

(defun sc-send (s) (with-current-buffer sc-buf (embterm-send-string s)))

(add-to-list 'load-path (expand-file-name "target/dist" sc-root))
(require 'embterm)
(set-frame-size nil 120 36)
(add-to-list 'embterm-shell-commands '("record" . sc-record))

(defun sc-native (f) (subst-char-in-string ?/ ?\\ (directory-file-name f)))

(defun sc-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (sc-check "エラーなし" nil (format "%S" err))))
                   (sc-run (cdr steps))))))

(sc-run
 `((0.5 . ,(lambda ()
             (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
               (setq sc-buf (embterm-new)))
             (delete-other-windows)))
   (3.5 . ,(lambda ()
             (sc-send (format ". '%s'\r" (concat (sc-native sc-etc) "\\embterm.ps1")))
             (sc-send (format "Set-Location '%s'\r" (sc-native sc-dir)))
             (sc-send "embterm_find_file 'あ b.txt'\r")
             (sc-send "embterm_cmd record 'x\"y' 'a\\b' 'あ'\r")))
   (2.5 . ,(lambda ()
             (let ((b (find-buffer-visiting (expand-file-name "あ b.txt" sc-dir))))
               (sc-check "pwsh: embterm_find_file で日本語と空白のパスが開く" b)
               (sc-check "別のウィンドウに出る" (and b (get-buffer-window b) (get-buffer-window sc-buf)))
               (when b (kill-buffer b)))
             (delete-other-windows (get-buffer-window sc-buf))
             (sc-check "pwsh: \" と \\ と日本語がそのまま届く" (equal (car sc-calls) '("x\"y" "a\\b" "あ")) (format "%S" sc-calls))
             (setq sc-calls nil)
             ;; 許可していない関数・長すぎる中身
             (sc-send (format "embterm_cmd delete-file '%s'\r" (sc-native (expand-file-name "keep.txt" sc-out))))
             (sc-send "embterm_cmd record ('z' * 9000)\r")))
   (2.0 . ,(lambda ()
             (sc-check "許可していない関数は呼ばれない" (file-exists-p (expand-file-name "keep.txt" sc-out)))
             (sc-check "長すぎて切れたものは捨てる" (null sc-calls) (format "%S" (mapcar (lambda (c) (length (car c))) sc-calls)))
             (setq embterm-allow-shell-commands nil)
             (sc-send "embterm_cmd record off\r")))
   (1.5 . ,(lambda ()
             (sc-check "embterm-allow-shell-commands が nil なら呼ばれない" (null sc-calls) (format "%S" sc-calls))
             (setq embterm-allow-shell-commands t)
             (kill-buffer sc-buf)
             ;; LANG が無いと Git Bash は端末からの日本語の入力を UTF-8 として読まない（ふだんは ~/.bashrc などで設定する）
             (let ((embterm-shell (list sc-git-bash "--noprofile" "--norc" "-i"))
                   (process-environment (cons "LANG=C.UTF-8" process-environment)))
               (setq sc-buf (embterm-new)))))
   (3.0 . ,(lambda ()
             (sc-send (format ". '%s'\r" (concat (file-name-as-directory sc-etc) "embterm.bash")))
             (sc-send (format "cd \"$(cygpath -u '%s')\"\r" (sc-native sc-dir)))
             (sc-send "embterm_dired .\r")
             (sc-send "embterm_cmd record 'bash \"q\"' 'c:\\x'\r")))
   (2.5 . ,(lambda ()
             (let ((d (seq-find (lambda (b) (with-current-buffer b
                                               (and (derived-mode-p 'dired-mode)
                                                    (equal (file-truename default-directory) (file-truename sc-dir)))))
                                (buffer-list))))
               (sc-check "Git Bash: embterm_dired でカレントディレクトリの dired が開く" d)
               (when d (kill-buffer d)))
             (sc-check "Git Bash: 引数がそのまま届く" (equal (car sc-calls) '("bash \"q\"" "c:\\x")) (format "%S" sc-calls))
             (setq sc-calls nil)
             (kill-buffer sc-buf)
             (let ((embterm-shell '("nyagos")))
               (setq sc-buf (embterm-new)))))
   (3.0 . ,(lambda ()
             (sc-send (format "lua_e \"dofile([[%s]])\"\r" (concat (sc-native sc-etc) "\\embterm.lua")))
             (sc-send "embterm_cmd record nyagos あい\r")))
   (2.0 . ,(lambda ()
             (sc-check "nyagos: embterm_cmd" (equal (car sc-calls) '("nyagos" "あい")) (format "%S" sc-calls))
             (sc-log "失敗の数: %d" sc-ng)
             (kill-emacs (if (zerop sc-ng) 0 1))))))

(run-at-time 60 nil (lambda () (sc-log "時間切れ") (kill-emacs 2)))

;;; shellcmd.el ends here
