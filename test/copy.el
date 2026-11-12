;;; copy.el --- copy モード（T-086）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/copy.el
;;
;; 結果は target/t086/copy.log（OK / NG）。最後に Emacs を終える。
;; 確かめること:
;;   - 1 MiB を超えるスクロールバックが分けて届き、全部の行がバッファに入る。受け取る間も Emacs が止まらない
;;   - バッファの行は端末の行のまま（端末が折り返した行も別の行）。折り返しの改行には `embterm-wrap' が付く
;;   - `filter-buffer-substring'（M-w など）では折り返しがつながる。本当の改行は残る
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst copy-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst copy-out (expand-file-name "target/t086/" copy-root))
(defconst copy-log-file (expand-file-name "copy.log" copy-out))

(make-directory copy-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p copy-log-file) (delete-file copy-log-file))

(defvar copy-ng 0)
(defvar copy-buf nil)
(defvar copy-gap-max 0)
(defvar copy-gap-last nil)
(defvar copy-gap-timer nil)

(defun copy-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil copy-log-file t 'silent)))

(defun copy-check (name ok &optional detail)
  (unless ok (cl-incf copy-ng))
  (copy-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun copy-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (copy-log "--- %s" name)
                     (condition-case err (funcall fn)
                       (error (copy-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (copy-run (cdr steps)))))))

(defconst copy-long (concat "LONG" (make-string 150 ?x) "END"))
(defconst copy-lines 16000 "1 行 80 文字ほどで 1.3 MB ほど")

(defconst copy-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 100 30)
              (load (expand-file-name "target/dist/embterm.el" copy-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-copy.log" copy-out))
              (setq embterm-scrollback-lines 50000)
              (let ((embterm-shell '("cmd.exe" "/d" "/q" "/k" "prompt $g")))
                (setq copy-buf (embterm-new)))
              (delete-other-windows)))
    (2.5 "長い行と、たくさんの行を出す"
         . ,(lambda ()
              (with-current-buffer copy-buf
                (embterm-send-string (format "echo %s\r" copy-long))
                (embterm-send-string
                 (format "for /l %%i in (1,1,%d) do @echo row%%i-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz-0123456789\r"
                         copy-lines)))))
    (12.0 "出し終えた → copy モード"
         . ,(lambda ()
              (copy-check "最後の行まで出た"
                          (string-match-p (format "row%d-" copy-lines) (with-current-buffer copy-buf (embterm-screen-text))))
              (setq copy-gap-last (float-time) copy-gap-max 0
                    copy-gap-timer (run-at-time 0 0.01 (lambda ()
                                                         (let ((now (float-time)))
                                                           (setq copy-gap-max (max copy-gap-max (- now copy-gap-last))
                                                                 copy-gap-last now)))))
              (with-current-buffer copy-buf (embterm-copy-mode))))
    (4.0 "copy モードのバッファ"
         . ,(lambda ()
              (cancel-timer copy-gap-timer)
              (copy-log "受け取る間のタイマーの間隔の最大: %.0f ms" (* 1000 copy-gap-max))
              (with-current-buffer copy-buf
                (let ((lines (count-lines (point-min) (point-max)))
                      (chars (buffer-size)))
                  (copy-log "行 %d / 文字 %d" lines chars)
                  (copy-check "1 MiB を超えるテキストが入った" (> chars (* 1024 1024)) (format "%d" chars))
                  (copy-check "最初と最後の行がある"
                              (save-excursion
                                (goto-char (point-min))
                                (and (search-forward "row1-" nil t)
                                     (search-forward (format "row%d-" copy-lines) nil t))))
                  (copy-check "受け取り終えた（途中の状態が残っていない）" (null embterm--copy-filling))
                  (copy-check "受け取る間も Emacs が止まらない（タイマーの間隔 300 ms 未満）" (< copy-gap-max 0.3)
                              (format "%.0f ms" (* 1000 copy-gap-max))))
                ;; 長い行（端末は 100 桁ほどで折り返す）
                (goto-char (point-min))
                (let* ((start (and (search-forward "LONG" nil t) (match-beginning 0)))
                       (end (and start (search-forward "END" nil t)))
                       (raw (and start (buffer-substring-no-properties start end)))
                       (filtered (and start (filter-buffer-substring start end))))
                  (copy-log "長い行: バッファ %S" raw)
                  (copy-check "バッファでは長い行が端末の行のまま分かれている" (and raw (string-search "\n" raw)))
                  (copy-check "折り返しの改行に embterm-wrap が付いている"
                              (and start (save-excursion
                                           (goto-char start) (end-of-line)
                                           (get-text-property (point) 'embterm-wrap))))
                  (copy-check "filter-buffer-substring では折り返しがつながる" (equal filtered copy-long) (format "%S" filtered)))
                ;; 本当の改行は残る
                (goto-char (point-min))
                (search-forward "row1-")
                (let* ((b (line-beginning-position)) (e (line-end-position 2))
                       (s (filter-buffer-substring b e)))
                  (copy-check "本当の改行は残る" (string-match-p "\\`row1-.*\nrow2-" s) (format "%S" s))))
              (copy-log "失敗の数: %d" copy-ng)
              (kill-emacs (if (zerop copy-ng) 0 1))))))

(run-at-time 90 nil (lambda () (copy-log "時間切れで終える") (kill-emacs 2)))
(copy-log "copy.el を始める（Emacs %s）" emacs-version)
(copy-run copy-steps)

;;; copy.el ends here
