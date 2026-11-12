;;; meta.el --- M-<非 ASCII の文字>（T-089）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/meta.el
;;
;; test/key-child.py を VT 入力（vt）と KKP（vt-kkp）で動かし、`M-a' と `M-あ' を本物のキー入力として積んで、
;; 子に届いたものを見る。結果は target/t089/meta.log（OK / NG）と child-*.log。最後に Emacs を終える。
;; `sit-for' は使わない

(require 'cl-lib)

(defconst meta-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst meta-out (expand-file-name "target/t089/" meta-root))
(defconst meta-log-file (expand-file-name "meta.log" meta-out))
(defconst meta-child (expand-file-name "test/key-child.py" meta-root))

(make-directory meta-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (directory-files meta-out t "\\.log\\'")) (delete-file f))

(defvar meta-ng 0)

(defun meta-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil meta-log-file t 'silent))

(defun meta-check (name ok &optional detail)
  (unless ok (cl-incf meta-ng))
  (meta-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun meta-child-log (mode)
  (let ((f (expand-file-name (format "child-%s.log" mode) meta-out)))
    (if (file-exists-p f) (with-temp-buffer (insert-file-contents f) (buffer-string)) "")))

(defun meta-spawn (mode)
  (let ((embterm-shell (list (or (executable-find "python") "python") meta-child mode
                             (expand-file-name (format "child-%s.log" mode) meta-out) "30")))
    (embterm-new)))

(defun meta-send ()
  (setq unread-command-events
        (append unread-command-events
                (list (event-apply-modifier ?a 'meta 27 "M-")
                      (event-apply-modifier ?あ 'meta 27 "M-")))))

(add-to-list 'load-path (expand-file-name "target/dist" meta-root))
(require 'embterm)
(set-frame-size nil 100 30)

(defun meta-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (meta-check "エラーなし" nil (format "%S" err))))
                   (meta-run (cdr steps))))))

(meta-run
 `((1.0 . ,(lambda ()
             (meta-check "M-あ は端末のキーマップで束縛されている"
                         (eq (lookup-key embterm-semi-char-map (vector (event-apply-modifier ?あ 'meta 27 "M-")))
                             'embterm--send-event))
             (meta-spawn "vt")))
   (2.5 . meta-send)
   (1.5 . ,(lambda ()
             (let ((l (meta-child-log "vt")))
               (meta-log "vt:\n%s" l)
               (meta-check "vt: M-a は ESC a" (string-search "RAW \\x1ba" l))
               (meta-check "vt: M-あ は ESC あ" (string-search "\\x1b\\u3042" l)))
             (kill-buffer (current-buffer))
             (meta-spawn "vt-kkp")))
   (2.5 . meta-send)
   (1.5 . ,(lambda ()
             (let ((l (meta-child-log "vt-kkp")))
               (meta-log "vt-kkp:\n%s" l)
               (meta-check "KKP: M-あ は Alt 付きの あ（12354;3u）" (string-search "12354;3u" l)))
             ;; embterm-reset-keyboard（T-090）で KKP のフラグを戻すと、レガシーの形（ESC あ）で届く
             (meta-check "embterm-module-reset-keyboard は戻すフラグがあれば t"
                         (embterm-module-reset-keyboard embterm--term))
             (meta-check "2 回目は戻すものが無い" (not (embterm-module-reset-keyboard embterm--term)))
             (meta-send)))
   (1.5 . ,(lambda ()
             (let ((l (meta-child-log "vt-kkp")))
               (meta-check "フラグを戻した後の M-あ は ESC あ" (string-search "\\x1b\\u3042" l)))
             (meta-log "失敗の数: %d" meta-ng)
             (kill-emacs (if (zerop meta-ng) 0 1))))))

(run-at-time 40 nil (lambda () (meta-log "時間切れ") (kill-emacs 2)))

;;; meta.el ends here
