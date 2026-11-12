;;; font.el --- フォントの設定（T-033）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/font.el
;;
;; 結果は target/t033/font.log（OK / NG）と画面（*.png）。最後に Emacs を終える。
;; 確かめること:
;;   - `embterm-font-family' に候補のリストを渡すと、見つかった最初のものを使う
;;   - `embterm-font-fallback-families' で、主フォントに無い文字（Nerd Font のアイコン）が描ける（画面は目で見る）
;;   - `text-scale-adjust' でそのバッファだけ大きくなり（桁数・行数が減る）、0 に戻すと元に戻る
;;   - `customize-set-variable' で開いている端末のフォントが変わる
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst font-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst font-out (expand-file-name "target/t033/" font-root))
(defconst font-log-file (expand-file-name "font.log" font-out))

(make-directory font-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p font-log-file) (delete-file font-log-file))

(defvar font-ng 0)
(defvar font-buf nil)
(defvar font-buf2 nil)
(defvar font-size0 nil)

(defun font-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil font-log-file t 'silent)))

(defun font-check (name ok &optional detail)
  (unless ok (cl-incf font-ng))
  (font-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun font-state (buf key) (plist-get (buffer-local-value 'embterm--state buf) key))
(defun font-size (buf) (cons (font-state buf :cols) (font-state buf :rows)))

(defun font-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (font-log "--- %s" name)
                     (condition-case err (funcall fn)
                       (error (font-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (font-run (cdr steps)))))))

(defconst font-steps
  `((1.0 "起動（候補のリストとフォールバック）"
         . ,(lambda ()
              (set-frame-size (selected-frame) 120 34)
              (load (expand-file-name "target/dist/embterm.el" font-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-font.log" font-out))
              (setq embterm-font-family '("No Such Font" "Cascadia Mono")
                    embterm-font-size 14
                    embterm-font-fallback-families '("No Such Fallback" "Symbols Nerd Font Mono"))
              (let ((embterm-shell '("cmd.exe")))
                (setq font-buf (embterm-new)))
              (delete-other-windows)))
    (3.0 "文字を出す"
         . ,(lambda ()
              (font-check "候補の 2 つ目（Cascadia Mono）を使う" (equal (font-state font-buf :font) "Cascadia Mono")
                          (format "%S" (font-state font-buf :font)))
              (setq font-size0 (font-size font-buf))
              (with-current-buffer font-buf
                (embterm-send-string (concat "echo 日本語 " (string #xe0a0 ?\s #xf07b ?\s #x1f600) " ①★\r")))))
    (1.5 "フォールバック（画面）"
         . ,(lambda ()
              (with-current-buffer font-buf (embterm-screenshot (expand-file-name "font-fallback.png" font-out)))
              ;; もう 1 つの端末を下に出し、text-scale はバッファごとであることを見る
              (let ((embterm-shell '("cmd.exe")))
                (save-selected-window
                  (select-window (split-window-below))
                  (setq font-buf2 (embterm-new))))
              (select-window (get-buffer-window font-buf))))
    (2.5 "text-scale-adjust +2"
         . ,(lambda ()
              (setq font-size0 (font-size font-buf))
              (font-log "前: %S / 下の端末 %S" font-size0 (font-size font-buf2))
              (with-current-buffer font-buf (text-scale-set 2))))
    (1.5 "大きくなった"
         . ,(lambda ()
              (let ((s (font-size font-buf)))
                (font-check "桁数・行数が減った（セルが大きくなった）"
                            (and (< (car s) (car font-size0)) (< (cdr s) (cdr font-size0)))
                            (format "%S → %S" font-size0 s)))
              (font-check "フォントは同じ" (equal (font-state font-buf :font) "Cascadia Mono"))
              (font-log "下の端末: %S" (font-size font-buf2))
              (with-current-buffer font-buf
                (embterm-send-string "echo scaled\r"))))
    (1.5 "大きくした後も描ける"
         . ,(lambda ()
              (with-current-buffer font-buf (embterm-screenshot (expand-file-name "font-scaled.png" font-out)))
              (font-check "大きくした後の出力" (string-match-p "^scaled" (with-current-buffer font-buf (embterm-screen-text))))
              (with-current-buffer font-buf (text-scale-set 0))))
    (1.5 "0 に戻した"
         . ,(lambda ()
              (font-check "元の桁数・行数に戻った" (equal (font-size font-buf) font-size0)
                          (format "%S / 元 %S" (font-size font-buf) font-size0))
              (customize-set-variable 'embterm-font-family "HackGen Console NF")))
    (1.5 "customize でフォントを替えた"
         . ,(lambda ()
              (font-check "上の端末が HackGen Console NF に" (equal (font-state font-buf :font) "HackGen Console NF")
                          (format "%S" (font-state font-buf :font)))
              (font-check "下の端末も HackGen Console NF に" (equal (font-state font-buf2 :font) "HackGen Console NF")
                          (format "%S" (font-state font-buf2 :font)))
              (with-current-buffer font-buf (embterm-screenshot (expand-file-name "font-hackgen.png" font-out)))))
    (1.0 "終わり"
         . ,(lambda ()
              (font-log "失敗の数: %d" font-ng)
              (kill-emacs (if (zerop font-ng) 0 1))))))

(run-at-time 60 nil (lambda () (font-log "時間切れで終える") (kill-emacs 2)))
(font-log "font.el を始める（Emacs %s）" emacs-version)
(font-run font-steps)

;;; font.el ends here
