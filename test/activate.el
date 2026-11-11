;;; activate.el --- 前面でないフレームの端末をクリックしたときの WM_MOUSEACTIVATE の確認 -*- lexical-binding: t; -*-
;; 使い方（**ユーザーが使っている Emacs には読み込まない**）:
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/activate.el
;; 結果は target/activate/activate.log。本物のマウスは動かさない（実際のクリックは手で確かめる）。

(defconst activate-root (expand-file-name ".." (file-name-directory load-file-name)))
(defconst activate-log (expand-file-name "target/activate/activate.log" activate-root))
(make-directory (file-name-directory activate-log) t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p activate-log) (delete-file activate-log))
(defun activate-write (s) (write-region (concat s "\n") nil activate-log t 'silent))

(load (expand-file-name "target/dist/embterm.el" activate-root))
(run-at-time 60 nil (lambda () (activate-write "時間切れ") (kill-emacs 1)))

(defvar activate-buf nil)
(run-at-time 1 nil (lambda () (let ((embterm-shell '("cmd.exe"))) (setq activate-buf (embterm)))))
(run-at-time
 5 nil
 (lambda ()
   (with-current-buffer activate-buf
     (embterm--poll-1)
     (let* ((pane (car (plist-get embterm--state :panes)))
            (hwnd (nth 1 pane)))
       (activate-write (format "pane=%S" pane))
       (with-temp-buffer
         (call-process "powershell" nil t nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                       (expand-file-name "test/mouse-activate.ps1" activate-root)
                       (number-to-string hwnd))
         (activate-write (string-trim (buffer-string))))
       (activate-write (format "frame-focus-state=%S" (frame-focus-state)))))
   (let ((kill-buffer-query-functions nil)) (kill-buffer activate-buf))
   (run-at-time 1 nil #'kill-emacs 0)))
;;; activate.el ends here
