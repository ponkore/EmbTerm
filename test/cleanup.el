;;; cleanup.el --- 後始末（T-041）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   bash test/cleanup.sh        # Emacs を起動し、最後に外から強制終了して残りを確かめる
;;
;; 結果は target/t041/cleanup.log（OK / NG）。test/close.el（kill-buffer・kill-emacs の閉じ方）で確かめていない場面:
;;   - シェルが自分で終わる（`exit'）と、バッファが消え、その端末の OpenConsole.exe も終わる
;;   - `embterm-kill-buffer-on-exit' が nil なら、バッファは「[終了]」で残り、あとで消せる
;;   - Emacs が異常終了する（`kill-emacs-hook' が走らない）。ここでは子と OpenConsole.exe の PID を
;;     target/t041/crash-pids.txt に書いて ready を置き、外（cleanup.sh）が taskkill /F で止めて残りを見る
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst cleanup-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst cleanup-out (expand-file-name "target/t041/" cleanup-root))
(defconst cleanup-log-file (expand-file-name "cleanup.log" cleanup-out))
(defconst cleanup-pids-file (expand-file-name "crash-pids.txt" cleanup-out))
(defconst cleanup-ready-file (expand-file-name "ready" cleanup-out))

(make-directory cleanup-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (list cleanup-log-file cleanup-pids-file cleanup-ready-file))
  (when (file-exists-p f) (delete-file f)))

(defvar cleanup-ng 0)

(defun cleanup-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil cleanup-log-file t 'silent)))

(defun cleanup-check (name ok &optional detail)
  (unless ok (cl-incf cleanup-ng))
  (cleanup-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun cleanup-open-consoles ()
  "この Emacs の子の OpenConsole.exe（と conhost.exe）の PID。"
  (let ((me (emacs-pid)))
    (seq-filter (lambda (pid)
                  (let ((a (process-attributes pid)))
                    (and (eql (alist-get 'ppid a) me)
                         (member (downcase (or (alist-get 'comm a) "")) '("openconsole.exe" "conhost.exe")))))
                (list-system-processes))))

(defun cleanup-pids (buf)
  "BUF の端末の子と子孫の PID（`名前(PID)' の並びから）。"
  (let ((r (with-current-buffer buf (embterm-module-process-report embterm--term))) (pos 0) pids)
    (while (string-match "(\\([0-9]+\\))" r pos)
      (push (string-to-number (match-string 1 r)) pids)
      (setq pos (match-end 0)))
    (nreverse pids)))

(defun cleanup-alive-p (pid) (and (process-attributes pid) t))

(defun cleanup-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (cleanup-log "--- %s" name)
                     (condition-case err (funcall fn)
                       (error (cleanup-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (cleanup-run (cdr steps)))))))

(defvar cleanup-buf nil)
(defvar cleanup-consoles-before nil)
(defvar cleanup-child-pids nil)
(defvar cleanup-bufs nil)

(defconst cleanup-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 100 30)
              (load (expand-file-name "target/dist/embterm.el" cleanup-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-cleanup.log" cleanup-out))
              (setq cleanup-consoles-before (cleanup-open-consoles))
              (let ((embterm-shell '("cmd.exe")))
                (setq cleanup-buf (embterm-new)))))
    (2.5 "cmd で exit"
         . ,(lambda ()
              (setq cleanup-child-pids (cleanup-pids cleanup-buf))
              (cleanup-log "子: %S / OpenConsole: %S" cleanup-child-pids (cleanup-open-consoles))
              (cleanup-check "OpenConsole.exe が 1 つ増えた" (= (length (cleanup-open-consoles)) (1+ (length cleanup-consoles-before)))
                             (format "%S → %S" cleanup-consoles-before (cleanup-open-consoles)))
              (with-current-buffer cleanup-buf (embterm-send-string "exit\r"))))
    (2.0 "exit の後"
         . ,(lambda ()
              (cleanup-check "バッファが消えた（embterm-kill-buffer-on-exit t）" (not (buffer-live-p cleanup-buf)))
              (cleanup-check "cmd が止まった" (null (seq-filter #'cleanup-alive-p cleanup-child-pids)))
              (cleanup-check "OpenConsole.exe が残っていない" (equal (cleanup-open-consoles) cleanup-consoles-before)
                             (format "%S" (cleanup-open-consoles)))
              (setq embterm-kill-buffer-on-exit nil)
              (let ((embterm-shell '("cmd.exe")))
                (setq cleanup-buf (embterm-new)))))
    (2.5 "embterm-kill-buffer-on-exit nil で exit"
         . ,(lambda ()
              (setq cleanup-child-pids (cleanup-pids cleanup-buf))
              (with-current-buffer cleanup-buf (embterm-send-string "echo bye & exit 3\r"))))
    (2.0 "バッファは残る"
         . ,(lambda ()
              (cleanup-check "バッファが残った" (buffer-live-p cleanup-buf))
              (cleanup-check "[終了] の印" (and (buffer-live-p cleanup-buf) (buffer-local-value 'embterm--exited cleanup-buf)))
              (cleanup-check "終わった後も画面が読める（最後の出力）"
                             (and (buffer-live-p cleanup-buf)
                                  (string-match-p "bye" (or (with-current-buffer cleanup-buf (embterm-screen-text)) ""))))
              (cleanup-check "cmd が止まった" (null (seq-filter #'cleanup-alive-p cleanup-child-pids)))
              (cleanup-check "OpenConsole.exe が残っていない" (equal (cleanup-open-consoles) cleanup-consoles-before)
                             (format "%S" (cleanup-open-consoles)))
              (kill-buffer cleanup-buf)
              (cleanup-check "終わった端末のバッファを消せる" (not (buffer-live-p cleanup-buf)))))
    (1.0 "異常終了の準備: cmd（ping）と pwsh（Store。Start-Process の子）を開く"
         . ,(lambda ()
              (setq embterm-kill-buffer-on-exit t)
              (let ((embterm-shell '("cmd.exe")))
                (push (embterm-new) cleanup-bufs))
              (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
                (push (embterm-new) cleanup-bufs))))
    (3.0 "子を起こす"
         . ,(lambda ()
              (with-current-buffer (cadr cleanup-bufs)
                (embterm-send-string "ping -n 60 127.0.0.1\r"))
              (with-current-buffer (car cleanup-bufs)
                (embterm-send-string "Start-Process -NoNewWindow ping -ArgumentList '-n','61','127.0.0.1'\r"))))
    (3.0 "PID を書いて、外からの強制終了を待つ"
         . ,(lambda ()
              (let* ((children (apply #'append (mapcar #'cleanup-pids cleanup-bufs)))
                     (consoles (seq-difference (cleanup-open-consoles) cleanup-consoles-before)))
                (cleanup-log "子孫: %S / OpenConsole: %S" children consoles)
                (dolist (b cleanup-bufs)
                  (cleanup-log "木: %s" (with-current-buffer b (embterm-module-process-report embterm--term))))
                (cleanup-check "ping が 2 つとも動いている"
                               (= 2 (cl-count-if (lambda (p) (string-equal-ignore-case (or (alist-get 'comm (process-attributes p)) "") "ping.exe")) children))
                               (format "%S" (mapcar (lambda (p) (alist-get 'comm (process-attributes p))) children)))
                (write-region (mapconcat #'number-to-string (append children consoles) " ") nil cleanup-pids-file nil 'silent)
                (cleanup-log "失敗の数: %d" cleanup-ng)
                (write-region (number-to-string (emacs-pid)) nil cleanup-ready-file nil 'silent))))))

(run-at-time 90 nil (lambda () (cleanup-log "時間切れで終える") (kill-emacs 2)))
(cleanup-log "cleanup.el を始める（Emacs %s）" emacs-version)
(cleanup-run cleanup-steps)

;;; cleanup.el ends here
