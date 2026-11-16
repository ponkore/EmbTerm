;;; ime.el --- IME の変換ウィンドウの位置（T-010 / T-137）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/ime.el
;;
;; tr-ime は -Q の Emacs には無いので、`tr-ime-modadv--set-composition-window' の**代わり**を置いて
;; 渡された引数を記録する。端末は test/key-child.py の vt モード（届いたキーを記録するだけでエコーしない）で
;; 動かすので、こちらが動かさないかぎりカーソルは止まったままになる。
;; 結果は target/t137/ime.log（OK / NG）。`sit-for' は使わない
;;
;; 確かめること:
;;   - 端末を選んでいる間は、変換ウィンドウの位置が端末のカーソル（ウィンドウの左上 + module の矩形）になる
;;   - 位置を渡したら `embterm-module-ime-refresh' で IME に反映させる（T-137 の (a)）
;;   - 確定した文字を送った直後は、エコーを待たずに表示幅ぶん先へ進める（T-137 の (b)）。
;;     本物のカーソルが動いたら・時間切れなら・折り返す位置なら、先読みを捨てる
;;   - 端末から離れたら上書きをやめる

(require 'cl-lib)

(defconst ime-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst ime-out (expand-file-name "target/t137/" ime-root))
(defconst ime-log-file (expand-file-name "ime.log" ime-out))
(defconst ime-child (expand-file-name "test/key-child.py" ime-root))

(make-directory ime-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (directory-files ime-out t "\\.log\\'")) (delete-file f))

(defvar ime-ng 0)
(defvar ime-buf nil)
(defvar ime-calls nil "代わりの `tr-ime-modadv--set-composition-window' が受けた引数（新しいものが先頭）。")
(defvar ime-refreshes 0 "`embterm-module-ime-refresh' を呼んだ回数。")
(defvar ime-advances 0 "`embterm--ime-advance' を呼んだ回数。")

(defun ime-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil ime-log-file t 'silent))

(defun ime-check (name ok &optional detail)
  (unless ok (cl-incf ime-ng))
  (ime-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

;; tr-ime の代わり（引数を記録するだけ）
(defun tr-ime-modadv--set-composition-window (hwnd style x y left top right bottom)
  (push (list hwnd style x y left top right bottom) ime-calls))

(defun ime-last-call ()
  (car ime-calls))

(defun ime-fresh-call ()
  "上書きを忘れさせてから `embterm--update-ime' を呼び、渡された引数を返す。"
  (setq ime-calls nil embterm--ime-last nil)
  (embterm--update-ime)
  (ime-last-call))

(defun ime-cursor ()
  (embterm-module-cursor embterm--term))

(defun ime-window-id ()
  (string-to-number (frame-parameter (selected-frame) 'window-id)))

(add-to-list 'load-path (expand-file-name "target/dist" ime-root))
(require 'embterm)
(set-frame-size nil 100 30)

(advice-add 'embterm--ime-advance :before (lambda (&rest _) (cl-incf ime-advances)))

(defun ime-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (ime-check "エラーなし" nil (format "%S" err))))
                   (ime-run (cdr steps))))))

(ime-run
 `((0.5 . ,(lambda ()
             (ime-check "tr-ime の代わりがあるので位置合わせは有効" (embterm--ime-available-p))
             (let ((embterm-shell (list (or (executable-find "python") "python") ime-child "vt"
                                        (expand-file-name "child.log" ime-out) "30")))
               (setq ime-buf (embterm-new)))))
   (2.5 . ,(lambda ()
             ;; module の関数は端末を開いたときに読み込まれるので、advice はここで掛ける
             (advice-add 'embterm-module-ime-refresh :before (lambda (&rest _) (cl-incf ime-refreshes)))
             (with-current-buffer ime-buf
               ;; --- 位置合わせの土台（T-010）
               (let ((rect (ime-cursor))
                     (edges (window-inside-pixel-edges (selected-window))))
                 (ime-check "端末のカーソルの矩形が取れる" (consp rect) (format "%S" rect))
                 (pcase-let ((`(,x ,y ,_w ,_h) rect)
                             (`(,l ,tp ,r ,b) edges))
                   (let ((call (ime-fresh-call)))
                     (ime-check "変換ウィンドウの位置は端末のカーソル"
                                (equal call (list (ime-window-id) 1 (+ l x) (+ tp y) l tp r b))
                                (format "%S" call)))))
               ;; --- (a) 入れた位置をその場で IME に反映させる
               (let ((n ime-refreshes))
                 (ime-fresh-call)
                 (ime-check "位置を渡したら ime-refresh も呼ぶ（a）" (> ime-refreshes n)
                            (format "%d → %d" n ime-refreshes)))
               (ime-check "ime-refresh はフレームの HWND に投げられる" (eq t (embterm-module-ime-refresh (ime-window-id))))
               (ime-check "ime-refresh は HWND が 0 なら nil" (null (embterm-module-ime-refresh 0)))
               (ime-check "ime-refresh は無いウィンドウなら nil" (null (embterm-module-ime-refresh 1))))))
   (0.3 . ,(lambda ()
             (with-current-buffer ime-buf
               ;; --- (b) 確定した文字の先読み
               (pcase-let* ((`(,x0 ,_y0 ,w ,_h) (ime-cursor))
                            (l (car (window-inside-pixel-edges (selected-window)))))
                 (setq embterm--ime-ahead nil ime-calls nil embterm--ime-last nil)
                 (embterm--ime-advance ?あ)
                 (ime-check "全角 1 文字で 2 セル進む（b）"
                            (equal (nth 2 (ime-last-call)) (+ l x0 (* 2 w)))
                            (format "x0=%s w=%s → %S" x0 w (nth 2 (ime-last-call))))
                 (embterm--ime-advance ?の)
                 (ime-check "続けて送ると足し合わさる（b）"
                            (equal (nth 2 (ime-last-call)) (+ l x0 (* 4 w))))
                 (embterm--ime-advance ?a)
                 (ime-check "半角は 1 セル" (equal (nth 2 (ime-last-call)) (+ l x0 (* 5 w))))
                 ;; 制御文字は画面に出ないので先読みしない
                 (let ((ahead embterm--ime-ahead))
                   (embterm--ime-advance ?\C-a)
                   (ime-check "制御文字では先読みしない" (equal ahead embterm--ime-ahead)))
                 ;; 本物のカーソルが動いたら捨てる（矩形が変わったことにする）
                 (setq embterm--ime-ahead (list '(-1 -1 1 1) (* 99 w) (float-time)))
                 (ime-check "カーソルが動いたら先読みを捨てる"
                            (and (equal (embterm--ime-cursor 10000) (ime-cursor)) (null embterm--ime-ahead)))
                 ;; 時間切れ
                 (setq embterm--ime-ahead (list (ime-cursor) (* 2 w) (- (float-time) (* 2 embterm--ime-ahead-timeout))))
                 (ime-check "古い先読みは捨てる"
                            (and (equal (embterm--ime-cursor 10000) (ime-cursor)) (null embterm--ime-ahead)))
                 ;; 折り返す位置（ウィンドウの幅を越える）は使わない
                 (setq embterm--ime-ahead (list (ime-cursor) 100000 (float-time)))
                 (ime-check "折り返す先読みは使わない" (equal (embterm--ime-cursor 500) (ime-cursor)))
                 (setq embterm--ime-ahead nil)))))
   (0.3 . ,(lambda ()
             (with-current-buffer ime-buf
               ;; --- 確定文字を送る道（`self-insert-command' の remap）から先読みが走る
               (let ((n ime-advances))
                 (let ((last-command-event ?あ)) (embterm--self-insert))
                 (ime-check "確定文字を送ると先読みが走る（b）" (> ime-advances n)))
               ;; --- copy モード（端末を隠して Emacs のバッファを見ている）では先読みしない
               (setq embterm--ime-ahead nil)
               (let ((embterm--input-mode 'copy))
                 (embterm--ime-advance ?あ)
                 (ime-check "copy モードでは先読みしない" (null embterm--ime-ahead))))))
   (0.5 . ,(lambda ()
             ;; --- 端末から離れたら上書きをやめる
             (setq ime-calls nil)
             (switch-to-buffer (get-buffer-create "*ime-other*"))
             (embterm--update-ime)
             (ime-check "端末を離れたら上書きをやめる（STYLE 0）"
                        (equal (cdr (ime-last-call)) '(0 0 0 0 0 0 0))
                        (format "%S" (ime-last-call)))
             (ime-check "離れたら先読みも消える" (null embterm--ime-ahead))))
   (0.5 . ,(lambda ()
             (when (buffer-live-p ime-buf) (kill-buffer ime-buf))
             (ime-log "失敗の数: %d" ime-ng)
             (kill-emacs (if (zerop ime-ng) 0 1))))))

(run-at-time 60 nil (lambda () (ime-log "時間切れ") (kill-emacs 2)))

;;; ime.el ends here
