;;; scheme.el --- 配色（ダーク / ライト）の知らせ（T-102）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/scheme.el
;;
;; test/key-child.py の scheme（CSI ?996n で問い合わせ、mode 2031 を立てる）を動かし、Emacs のテーマを
;; ダーク（modus-vivendi）にして戻す。結果は target/t102/scheme.log（OK / NG）と child-scheme.log。
;; `sit-for' は使わない

(require 'cl-lib)

(defconst scheme-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst scheme-out (expand-file-name "target/t102/" scheme-root))
(defconst scheme-log-file (expand-file-name "scheme.log" scheme-out))
(defconst scheme-child-log (expand-file-name "child-scheme.log" scheme-out))

(make-directory scheme-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (list scheme-log-file scheme-child-log)) (when (file-exists-p f) (delete-file f)))

(defvar scheme-ng 0)

(defun scheme-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil scheme-log-file t 'silent))

(defun scheme-check (name ok &optional detail)
  (unless ok (cl-incf scheme-ng))
  (scheme-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun scheme-child ()
  (if (file-exists-p scheme-child-log)
      (with-temp-buffer (insert-file-contents scheme-child-log) (buffer-string))
    ""))

(defun scheme-count (re)
  (let ((text (scheme-child)) (pos 0) (n 0))
    (while (string-match re text pos) (setq n (1+ n) pos (match-end 0)))
    n))

(add-to-list 'load-path (expand-file-name "target/dist" scheme-root))
(require 'embterm)
(set-frame-size nil 100 30)

(defun scheme-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (scheme-check "エラーなし" nil (format "%S" err))))
                   (scheme-run (cdr steps))))))

(scheme-run
 `((0.5 . ,(lambda ()
             (let ((embterm-shell (list (or (executable-find "python") "python")
                                        (expand-file-name "test/key-child.py" scheme-root) "scheme" scheme-child-log "30")))
               (embterm-new))))
   (3.0 . ,(lambda ()
             (scheme-log "子:\n%s" (scheme-child))
             (scheme-check "問い合わせにはライトで答える（emacs -Q の白い背景）" (= 1 (scheme-count "997;2n")))
             (load-theme 'modus-vivendi t)))
   (1.5 . ,(lambda ()
             (scheme-log "子:\n%s" (scheme-child))
             (scheme-check "ダークのテーマにしたらダークを知らせる" (= 1 (scheme-count "997;1n")))
             (disable-theme 'modus-vivendi)))
   (1.5 . ,(lambda ()
             (scheme-log "子:\n%s" (scheme-child))
             (scheme-check "テーマを外したらライトを知らせる" (= 2 (scheme-count "997;2n")))
             (scheme-log "失敗の数: %d" scheme-ng)
             (kill-emacs (if (zerop scheme-ng) 0 1))))))

(run-at-time 40 nil (lambda () (scheme-log "時間切れ") (kill-emacs 2)))

;;; scheme.el ends here
