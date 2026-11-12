;;; bigimage.el --- 大きな画像を GPU に載せる UI スレッドの仕事（T-105）の測定と確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/bigimage.el
;;
;; cmd の端末で `kitty-graphics.py big`（4000x4000 の PNG を 40 桁 x 20 行に縮めて置く）を動かし、module の性能の記録
;; （`embterm-module-perf'）の ui.draw（1 回の描画）と ui.request_delay（頼みごとの待ち）の最大を見る。
;; 結果は target/t105/bigimage.log（OK / NG）と、画像の画面 bigimage.png。`sit-for' は使わない

(require 'cl-lib)

(defconst big-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst big-out (expand-file-name "target/t105/" big-root))
(defconst big-log-file (expand-file-name "bigimage.log" big-out))

(make-directory big-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p big-log-file) (delete-file big-log-file))

(defvar big-ng 0)
(defvar big-buf nil)

(defun big-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil big-log-file t 'silent))

(defun big-check (name ok &optional detail)
  (unless ok (cl-incf big-ng))
  (big-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun big-perf-max (report key)
  "REPORT の KEY の行の max（ms）。"
  (and (string-match (concat "^" (regexp-quote key) " .*max=\\([0-9.]+\\)") report)
       (string-to-number (match-string 1 report))))

(add-to-list 'load-path (expand-file-name "target/dist" big-root))
(require 'embterm)
(set-frame-size nil 120 34)

(defun big-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (big-check "エラーなし" nil (format "%S" err))))
                   (big-run (cdr steps))))))

(big-run
 `((0.5 . ,(lambda ()
             (let ((embterm-shell '("cmd.exe" "/d" "/q" "/k" "prompt $g")))
               (setq big-buf (embterm-new)))
             (delete-other-windows)))
   (2.5 . ,(lambda ()
             (with-current-buffer big-buf
               (embterm-module-perf embterm--term t)
               (embterm-send-string (format "python -I %s/test/kitty-graphics.py big --no-wait\r" big-root)))))
   (12.0 . ,(lambda ()
              (with-current-buffer big-buf
                (let* ((report (embterm-module-perf embterm--term nil))
                       (info (embterm-module-debug-info embterm--term))
                       (draw (big-perf-max report "ui.draw"))
                       (delay (big-perf-max report "ui.request_delay")))
                  (big-log "性能:\n%s" report)
                  (big-log "画像: 変換が終わった回数 %S、変換中 %S" (plist-get info :images-converted) (plist-get info :images-pending))
                  (big-check "大きな画像は別のスレッドで変換した" (>= (plist-get info :images-converted) 1))
                  (big-check "変換中のものが残っていない" (= (plist-get info :images-pending) 0))
                  (big-check "UI スレッドの 1 回の描画の最大が 30 ms 未満（前は 60〜140 ms）" (and draw (< draw 30)) (format "%S ms" draw))
                  (big-log "頼みごとの待ちの最大: %S ms" delay)
                  (embterm-screenshot (expand-file-name "bigimage.png" big-out))))))
   (1.5 . ,(lambda ()
             (big-log "失敗の数: %d" big-ng)
             (kill-emacs (if (zerop big-ng) 0 1))))))

(run-at-time 90 nil (lambda () (big-log "時間切れ") (kill-emacs 2)))

;;; bigimage.el ends here
