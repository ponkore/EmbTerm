;;; close.el --- 閉じ方（T-076 / T-073）と GPU デバイスの共有（T-067）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/close.el
;;   # Emacs が終わったあとで、target/t054/close-pids.txt の PID が残っていないこと・swap が無いことを外から確かめる
;;
;; 結果は target/t054/close.log（OK / NG）と module-close.log。最後に `kill-emacs' で終える（`embterm-kill-emacs-timeout' の経路）。
;; 確かめること:
;;   - バッファを消すと待たずに戻り、Job の中の CTRL_CLOSE_EVENT を受けないプロセス（pythonw）は猶予の後に止まる
;;   - パッケージのアプリ（Microsoft Store の pwsh）が起こした子が Job に入っている（T-073）
;;   - GPU デバイスは端末が何個でも 1 つ。失ったことにすると全部の端末が新しいデバイスに載せ替えて描ける（T-067）
;;   - Emacs を終えると、中の vim（scoop の Windows 版）が穏やかに終わり swap が残らない
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst close-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst close-out (expand-file-name "target/t054/" close-root))
(defconst close-log-file (expand-file-name "close.log" close-out))
(defconst close-pids-file (expand-file-name "close-pids.txt" close-out))
(defconst close-work (expand-file-name "target/t054/closework/" close-root))
(defconst close-vim
  (or (getenv "VIM_EXE")
      (executable-find "vim")
      (seq-find #'file-executable-p
                (list (expand-file-name "scoop/shims/vim.exe" (getenv "USERPROFILE"))
                      (expand-file-name "scoop/apps/vim/current/vim.exe" (getenv "USERPROFILE"))
                      "C:/Program Files/Vim/vim91/vim.exe"
                      "C:/Program Files (x86)/Vim/vim91/vim.exe"))
      "vim")
  "vim の実行ファイル。環境変数 VIM_EXE → PATH → よくある置き場 の順に探す。")

(make-directory close-out t)
(make-directory close-work t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (append (list close-log-file close-pids-file (expand-file-name "module-close.log" close-out)) (directory-files close-work t "\\`[^.]\\|\\`\\.[^.]")))
  (when (file-regular-p f) (delete-file f)))

(defvar close-ng 0)

(defun close-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil close-log-file t 'silent)))

(defun close-check (name ok &optional detail)
  (unless ok (cl-incf close-ng))
  (close-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun close-report (buf)
  (with-current-buffer buf (embterm-module-process-report embterm--term)))

(defun close-pids (report)
  "REPORT（`名前(PID)' の並び）の PID。"
  (let ((pos 0) pids)
    (while (string-match "(\\([0-9]+\\))" report pos)
      (push (string-to-number (match-string 1 report)) pids)
      (setq pos (match-end 0)))
    (nreverse pids)))

(defun close-alive-p (pid) (and (process-attributes pid) t))

(defun close-perf-line (buf key)
  (with-current-buffer buf
    (let ((r (or (embterm-module-perf embterm--term nil) "")))
      (and (string-match (concat "^" (regexp-quote key) ".*$") r) (match-string 0 r)))))

(defun close-generation (buf)
  "BUF の端末のペインが使っている GPU デバイスの世代（最初のペイン）。"
  (let ((l (close-perf-line buf "gpu.")))
    (and l (string-match "generations=\\[\\([0-9]+\\)" l) (string-to-number (match-string 1 l)))))

(defvar close-gen-before nil)

(defun close-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (close-log "--- %s" name)
                     (condition-case err (funcall fn) (error (close-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (close-run (cdr steps)))))))

(defvar close-cmd nil)
(defvar close-pwsh nil)
(defvar close-vimbuf nil)
(defvar close-cmd-pids nil)

(defconst close-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 110 34)
              (load (expand-file-name "target/dist/embterm.el" close-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-close.log" close-out))
              (let ((embterm-shell '("cmd.exe")))
                (setq close-cmd (embterm-new)))))
    (3.0 "cmd: CTRL_CLOSE_EVENT を受けない子（pythonw）と、cmd → pwsh（Store）→ 隠れた ping"
         . ,(lambda ()
              (with-current-buffer close-cmd
                (embterm-send-string "start \"\" pythonw -c \"import time; time.sleep(40)\"\r")
                (embterm-send-string "pwsh -NoProfile -Command \"Start-Process ping -ArgumentList '-n','40','127.0.0.1' -WindowStyle Hidden; Start-Sleep 30\"\r"))))
    (5.0 "cmd の子の木"
         . ,(lambda ()
              (let ((r (close-report close-cmd)))
                (close-log "木: %s" r)
                (setq close-cmd-pids (cdr (close-pids r)))
                (close-check "cmd の子に pythonw がいる" (string-match-p "pythonw" r) r))
              (let ((t0 (float-time)))
                (kill-buffer close-cmd)
                (close-check "kill-buffer は待たない（10 ms 未満）" (< (- (float-time) t0) 0.01)
                             (format "%.1f ms" (* 1000 (- (float-time) t0)))))))
    (7.0 "猶予（5 秒）の後: cmd の子孫が残っていない"
         . ,(lambda ()
              (let ((alive (seq-filter #'close-alive-p close-cmd-pids)))
                (close-check "cmd の子孫（Job の中・外）がすべて止まった" (null alive) (format "%S / 残り %S" close-cmd-pids alive)))
              (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
                (setq close-pwsh (embterm-new)))))
    (4.0 "pwsh（Store）の子が Job に入る（T-073）"
         . ,(lambda ()
              (with-current-buffer close-pwsh
                (embterm-send-string "Start-Process -NoNewWindow ping -ArgumentList '-n','40','127.0.0.1'\r")
                (embterm-send-string "Start-Process ping -ArgumentList '-n','41','127.0.0.1' -WindowStyle Hidden\r"))))
    (3.0 "pwsh の子の木"
         . ,(lambda ()
              (let ((r (close-report close-pwsh)))
                (close-log "木: %s" r)
                (close-check "pwsh の子の ping が 2 つとも Job の中" (and (= (cl-count ?! r) 0) (>= (length (close-pids r)) 3)) r))
              (let ((embterm-shell (list close-vim "-N" "-u" "NONE" (expand-file-name "vimfile.txt" close-work))))
                (write-region "hello\n" nil (expand-file-name "vimfile.txt" close-work) nil 'silent)
                (setq close-vimbuf (embterm-new)))))
    (3.0 "vim を開いたまま（書き換えない）"
         . ,(lambda ()
              (close-log "swap: %S" (directory-files close-work nil "\\.sw"))))
    (1.0 "GPU デバイス: 端末 2 個で 1 つ。上下に並べて両方を出す"
         . ,(lambda ()
              (delete-other-windows)
              (switch-to-buffer close-pwsh)
              (set-window-buffer (split-window-below) close-vimbuf)))
    (1.5 "失ったことにする（T-067）"
         . ,(lambda ()
              (close-check "GPU デバイスは 1 つ" (= (embterm-module-gpu-devices) 1) (format "%s" (embterm-module-gpu-devices)))
              (setq close-gen-before (list (close-generation close-pwsh) (close-generation close-vimbuf)))
              (close-log "前: %s / %s" (close-perf-line close-pwsh "gpu.") (close-perf-line close-vimbuf "gpu."))
              (embterm-module-debug-lose-device)))
    (1.5 "載せ替えた後"
         . ,(lambda ()
              (let ((a (close-perf-line close-pwsh "gpu.")) (b (close-perf-line close-vimbuf "gpu.")))
                (close-log "後: %s / %s" a b)
                (close-check "両方の端末が新しい世代に載せ替えた"
                             (and (car close-gen-before) (cadr close-gen-before)
                                  (> (or (close-generation close-pwsh) 0) (car close-gen-before))
                                  (> (or (close-generation close-vimbuf) 0) (cadr close-gen-before))
                                  (eql (close-generation close-pwsh) (close-generation close-vimbuf)))
                             (format "%S → %s / %s" close-gen-before a b)))
              (close-check "デバイスは 1 つに戻った" (= (embterm-module-gpu-devices) 1) (format "%s" (embterm-module-gpu-devices)))
              (with-current-buffer close-pwsh (embterm-send-string "echo after-device-loss\r"))))
    (1.5 "載せ替えた後も描ける"
         . ,(lambda ()
              (let ((png (expand-file-name "close-after-device-loss.png" close-out)))
                (with-current-buffer close-pwsh (embterm-screenshot png)))
              (close-check "描画の失敗が無い" (not (string-match-p "描画に失敗"
                                                                (with-temp-buffer
                                                                  (insert-file-contents (expand-file-name "module-close.log" close-out))
                                                                  (buffer-string)))))))
    (1.5 "vim のバッファを消す（猶予 5 秒で穏やかに閉じる）"
         . ,(lambda ()
              (setq close-cmd-pids (close-pids (close-report close-vimbuf)))
              (kill-buffer close-vimbuf)))
    (6.0 "vim が終わった"
         . ,(lambda ()
              (close-check "vim が止まった" (null (seq-filter #'close-alive-p close-cmd-pids)) (format "%S" close-cmd-pids))
              (close-check "書き換えていない vim の swap が残っていない（Windows 版の vim は CTRL_CLOSE_EVENT で穏やかに終わり swap を消す）"
                           (null (directory-files close-work nil "\\.sw")) (format "%S" (directory-files close-work nil "\\.sw")))
              (let ((embterm-shell (list close-vim "-N" "-u" "NONE" (expand-file-name "vimfile2.txt" close-work))))
                (write-region "hello\n" nil (expand-file-name "vimfile2.txt" close-work) nil 'silent)
                (setq close-vimbuf (embterm-new)))))
    (3.0 "もう 1 つの vim で書き換える（Emacs を終えるときに閉じる）"
         . ,(lambda ()
              (with-current-buffer close-vimbuf
                (embterm-send-string "ichanged\e"))))
    (1.5 "Emacs を終える（kill-emacs）"
         . ,(lambda ()
              (close-log "書き換えた vim の swap（復旧のために残るはず）: %S" (directory-files close-work nil "\\.sw"))
              (let ((pids (append (close-pids (close-report close-pwsh)) (close-pids (close-report close-vimbuf)))))
                (write-region (mapconcat #'number-to-string pids " ") nil close-pids-file nil 'silent)
                (close-log "終える前の PID: %S / swap: %S" pids (directory-files close-work nil "\\.sw")))
              (close-log "NG の数: %d" close-ng)
              (close-log "kill-emacs（embterm-kill-emacs-timeout %s）" embterm-kill-emacs-timeout)
              (let ((t0 (float-time)))
                (add-hook 'kill-emacs-hook (lambda () (close-log "kill-emacs-hook の終わり: %.0f ms" (* 1000 (- (float-time) t0)))) 90))
              (kill-emacs 0)))))

(run-at-time 120 nil (lambda () (close-log "時間切れで終える") (kill-emacs 2)))
(close-log "close.el を始める（Emacs %s）" emacs-version)
(close-run close-steps)

;;; close.el ends here
