;;; resize.el --- リサイズと reflow（T-032）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/resize.el
;;
;; 結果は target/t032/resize.log（OK / NG）と、手順ごとの画面（*.png / *.txt）。最後に Emacs を終える。
;; 確かめること（pwsh + PSReadLine。ConPTY は再配布版）:
;;   - 狭めると長い行が折り返し直され（reflow）、広げるとつながる。中身が欠けない・重ならない
;;   - アプリ（pwsh）から見た大きさが端末と同じ
;;   - 狭めた・広げた後の入力と出力がプロンプトの下に正しく出る（ConPTY との食い違いが無い）
;;   - 続けざまに大きさを変えても（フレームのドラッグ）、最後の大きさに落ち着き、同じ出力が二重に出ない
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst resize-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst resize-out (expand-file-name "target/t032/" resize-root))
(defconst resize-log-file (expand-file-name "resize.log" resize-out))

(make-directory resize-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p resize-log-file) (delete-file resize-log-file))

(defvar resize-ng 0)
(defvar resize-buf nil)

(defun resize-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil resize-log-file t 'silent)))

(defun resize-check (name ok &optional detail)
  (unless ok (cl-incf resize-ng))
  (resize-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun resize-text ()
  (with-current-buffer resize-buf (or (embterm-screen-text) "")))

(defun resize-lines ()
  (seq-remove #'string-empty-p (mapcar #'string-trim-right (split-string (resize-text) "\n"))))

(defun resize-size ()
  "端末の桁数と行数（module の状態）。"
  (let ((st (buffer-local-value 'embterm--state resize-buf)))
    (cons (plist-get st :cols) (plist-get st :rows))))

(defun resize-save (name)
  (write-region (resize-text) nil (expand-file-name (concat name ".txt") resize-out) nil 'silent)
  (with-current-buffer resize-buf (embterm-screenshot (expand-file-name (concat name ".png") resize-out)))
  (resize-log "画面 %s（%dx%d）:\n%s" name (car (resize-size)) (cdr (resize-size))
              (mapconcat (lambda (l) (concat "  | " l)) (resize-lines) "\n")))

(defun resize-count (re)
  "画面に RE が何回出るか。"
  (let ((text (resize-text)) (pos 0) (n 0))
    (while (string-match re text pos)
      (setq n (1+ n) pos (match-end 0)))
    n))

(defun resize-joined ()
  "画面の行を改行なしでつないだもの（折り返し直された行がつながっているかを見る）。"
  (mapconcat #'identity (mapcar #'string-trim-right (split-string (resize-text) "\n")) ""))

(defconst resize-long (concat (make-string 90 ?A) (make-string 90 ?B)))

(defun resize-check-long (label)
  ;; 出力の行 = 出力の mark1 の直前に続く A/B だけの行（コマンドの折り返しにも A/B だけの行がある）
  (let* ((all (resize-lines))
         (i (seq-position all "mark1"))
         (lines (and i (nreverse (seq-take-while (lambda (l) (string-match-p "\\`[AB]+\\'" l))
                                                 (reverse (seq-take all i))))))
         (cols (car (resize-size))))
    (resize-check (format "%s: 長い行が欠けずにつながる" label)
                  (string-match-p (regexp-quote resize-long) (resize-joined)))
    (resize-check (format "%s: 長い行が %d 桁で折り返される（%d 行）" label cols (ceiling 180 cols))
                  (= (length lines) (ceiling 180 cols))
                  (format "%S" (mapcar #'length lines)))))

(defun resize-send (s) (with-current-buffer resize-buf (embterm-send-string s)))

(defun resize-check-app-size (label)
  "pwsh が答えた大きさ（`SIZE=WxH'）が端末と同じか。"
  (let* ((text (resize-text))
         (m (and (string-match "SIZE=\\([0-9]+\\)x\\([0-9]+\\)" text)
                 (cons (string-to-number (match-string 1 text)) (string-to-number (match-string 2 text))))))
    (resize-check (format "%s: アプリから見た大きさが端末と同じ" label) (equal m (resize-size))
                  (format "アプリ %S / 端末 %S" m (resize-size)))))

(defconst resize-size-command
  "cls; '{0}' -f ('SIZE=' + $Host.UI.RawUI.WindowSize.Width + 'x' + $Host.UI.RawUI.WindowSize.Height)\r")

(defun resize-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (resize-log "--- %s" name)
                     (condition-case err (funcall fn)
                       (error (resize-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (resize-run (cdr steps)))))))

(defvar resize-before nil)

(defconst resize-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 130 36)
              (set-frame-position (selected-frame) 40 40)
              (load (expand-file-name "target/dist/embterm.el" resize-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-resize.log" resize-out))
              (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
                (setq resize-buf (embterm-new)))
              (delete-other-windows)))
    (3.0 "大きさを尋ねる"
         . ,(lambda () (resize-send resize-size-command)))
    (1.5 "長い行を出す"
         . ,(lambda ()
              (resize-check-app-size "最初")
              (resize-send (format "'%s'; 'mark1'\r" resize-long))))
    (1.5 "最初の大きさ"
         . ,(lambda ()
              (resize-save "1-wide")
              (resize-check-long "最初")
              (setq resize-before (resize-size))
              (split-window-right)))
    (1.5 "左右に分けて狭めた"
         . ,(lambda ()
              (resize-save "2-narrow")
              (resize-check "桁数が減った" (< (car (resize-size)) (car resize-before)) (format "%S → %S" resize-before (resize-size)))
              (resize-check-long "狭めた後")
              (resize-check "mark1 は 1 回" (= 2 (resize-count "mark1")) (format "%d（コマンドの中と出力で 2 のはず）" (resize-count "mark1")))
              (resize-send "'after-' + 'narrow'\r")))
    (1.5 "狭めた後の入力"
         . ,(lambda ()
              (resize-save "3-narrow-input")
              (let ((lines (resize-lines)))
                (resize-check "出力がコマンドの次の行に出る"
                              (let ((i (seq-position lines "after-narrow")))
                                (and i (> i 0) (string-match-p "'after-' \\+ 'narrow'" (nth (1- i) lines))))
                              (format "%S" (last lines 4))))
              (delete-other-windows)))
    (1.5 "広げた"
         . ,(lambda ()
              (resize-save "4-wide-again")
              (resize-check "桁数が戻った" (equal (resize-size) resize-before) (format "%S" (resize-size)))
              (resize-check-long "広げた後")
              (resize-check "mark1 は 1 回" (= 2 (resize-count "mark1")) (format "%d" (resize-count "mark1")))
              (resize-send "'after-' + 'wide'\r")))
    (1.5 "広げた後の入力"
         . ,(lambda ()
              (resize-save "5-wide-input")
              (resize-check "after-wide が出た" (= 1 (resize-count "^after-wide")))
              ;; フレームのドラッグを真似る: 30 ms ごとに 20 回、大きさを変える
              (let ((sizes (cl-loop for i from 0 below 20 collect (cons (- 130 (* 3 i)) (- 36 (/ i 2))))))
                (cl-loop for (w . h) in sizes for i from 0
                         do (run-at-time (* i 0.03) nil
                                         (lambda () (set-frame-size (selected-frame) w h)))))))
    (2.5 "続けざまに変えた後"
         . ,(lambda ()
              (resize-save "6-storm")
              (let* ((win (get-buffer-window resize-buf))
                     (edges (window-inside-pixel-edges win)))
                (resize-log "ウィンドウ %S → 端末 %S" edges (resize-size)))
              (resize-check-long "続けざまに変えた後")
              (resize-check "mark1 は 1 回" (= 2 (resize-count "mark1")) (format "%d" (resize-count "mark1")))
              (resize-send resize-size-command)))
    (1.5 "続けざまに変えた後の大きさ"
         . ,(lambda ()
              (resize-check-app-size "続けざまに変えた後")
              (resize-send "'after-' + 'storm'\r")))
    (1.5 "続けざまに変えた後の入力"
         . ,(lambda ()
              (resize-save "7-storm-input")
              (resize-check "after-storm が 1 回出た" (= 1 (resize-count "^after-storm")))
              ;; 縦だけ縮めて戻す（行が画面の上に押し出され、戻すとスクロールバックから戻らない: resize_pull_scrollback = false）
              (resize-send "1..40 | % { \"line$_\" }\r")))
    (1.5 "40 行出した"
         . ,(lambda ()
              (resize-save "8-lines")
              (setq resize-before (resize-size))
              (set-frame-size (selected-frame) 100 15)))
    (1.5 "縦に縮めた"
         . ,(lambda ()
              (resize-save "9-short")
              (resize-check "行数が減った" (< (cdr (resize-size)) (cdr resize-before)) (format "%S" (resize-size)))
              (resize-check "最後の行（line40 とプロンプト）が見える"
                            (and (= 1 (resize-count "^line40$")) (string-prefix-p "PS " (car (last (resize-lines))))))
              (set-frame-size (selected-frame) 100 30)))
    (1.5 "縦に戻した"
         . ,(lambda ()
              (resize-save "10-tall")
              (resize-check "最後の行（line40 とプロンプト）が見える"
                            (and (= 1 (resize-count "^line40$")) (string-prefix-p "PS " (car (last (resize-lines))))))
              (resize-send "'after-' + 'tall'\r")))
    (1.5 "縦に戻した後の入力"
         . ,(lambda ()
              (resize-save "11-tall-input")
              (let ((lines (resize-lines)))
                (resize-check "出力がコマンドの次の行に出る"
                              (let ((i (seq-position lines "after-tall")))
                                (and i (> i 0) (string-match-p "'after-' \\+ 'tall'" (nth (1- i) lines))))
                              (format "%S" (last lines 4))))
              (resize-log "失敗の数: %d" resize-ng)
              (kill-emacs (if (zerop resize-ng) 0 1))))))

(run-at-time 90 nil (lambda () (resize-log "時間切れで終える") (kill-emacs 2)))
(resize-log "resize.el を始める（Emacs %s）" emacs-version)
(resize-run resize-steps)

;;; resize.el ends here
