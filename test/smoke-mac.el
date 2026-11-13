;;; smoke-mac.el --- macOS の module の自動確認（T-126） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   EMBTERM_ROOT=$PWD emacs -Q -l $PWD/test/smoke-mac.el
;;
;; target/dist の embterm.el と module を読み、/bin/sh の端末を開いてキーを送り、
;; ペインの配置・描画・画面のテキストを確かめて target/t126/ に書く:
;;   smoke-mac.log   手順ごとの状態（module の plist）
;;   *.png / *.txt   端末の画面（module が Metal で描いて保存したもの）
;;   module.log      module の中のログ
;; 合否は終了コード（0 = OK）。待ちは `run-at-time' で作る（`sit-for' は使わない）

(require 'cl-lib)
(require 'seq)

(defconst smoke-root
  ;; GUI の Emacs では default-directory が当てにならないので、環境変数か絶対パスの load-file-name から決める
  (or (getenv "EMBTERM_ROOT")
      (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst smoke-out (expand-file-name "target/t126/" smoke-root))
(defconst smoke-log-file (expand-file-name "smoke-mac.log" smoke-out))
(defvar smoke-failures nil)

(make-directory smoke-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p smoke-log-file)
  (delete-file smoke-log-file))

(defun smoke-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil smoke-log-file t 'silent)))

(defun smoke-out (name) (expand-file-name name smoke-out))

(defun smoke-check (ok fmt &rest args)
  (let ((msg (apply #'format fmt args)))
    (smoke-log "%s %s" (if ok "OK" "NG") msg)
    (unless ok (push msg smoke-failures))
    ok))

;; module と embterm.el は配布物から読む
(setq embterm-module-directory (expand-file-name "target/dist" smoke-root))
(load (expand-file-name "target/dist/embterm.el" smoke-root) nil t)
(setq embterm-shell '("/bin/sh"))
(setq embterm-poll-interval 0.05)

(defvar smoke-buf nil)
(defvar smoke-steps nil)

(defun smoke-state ()
  (when (buffer-live-p smoke-buf)
    (with-current-buffer smoke-buf
      (and embterm--term (embterm-module-poll embterm--term nil)))))

(defun smoke-next ()
  "次の手順を 0.9 秒後に動かす。"
  (if-let* ((step (pop smoke-steps)))
      (run-at-time 0.9 nil (lambda () (condition-case e (funcall step)
                                        (error (smoke-check nil "手順で error: %S" e) (smoke-finish)))))
    (smoke-finish)))

(defun smoke-finish ()
  (smoke-log "終わり（NG %d 件）" (length smoke-failures))
  (kill-emacs (if smoke-failures 1 0)))

(setq smoke-steps
      (list
       (lambda ()
         ;; 1. 端末を開く
         (embterm--load-module)
         (smoke-log "module の版: %s" embterm--module-loaded)
         (embterm-module-set-log (smoke-out "module.log"))
         (setq smoke-buf (embterm))
         (smoke-check (buffer-live-p smoke-buf) "端末のバッファができた: %s" smoke-buf)
         (redisplay t)
         (smoke-next))
       (lambda ()
         ;; 2. ペインが置かれ、子プロセスが起動した
         (let* ((s (smoke-state)))
           (smoke-log "状態: %S" s)
           (smoke-check (plist-get s :started) "子プロセスが起動した（pid %s）" (plist-get s :pid))
           (smoke-check (plist-get s :view) "ペインが置かれた")
           (smoke-check (> (plist-get s :cols) 10) "桁数 %s x 行数 %s" (plist-get s :cols) (plist-get s :rows)))
         (smoke-next))
       (lambda ()
         ;; 3. キーを送る（echo）
         (with-current-buffer smoke-buf
           (dolist (ch (string-to-list "printf 'hello from emacs\\n'"))
             (embterm-module-send-key embterm--term (char-to-string ch) nil))
           (embterm-module-send-key embterm--term "return" nil))
         (smoke-next))
       (lambda ()
         ;; 4. 画面のテキストに出たか
         (with-current-buffer smoke-buf
           (let ((text (embterm-module-text embterm--term)))
             (write-region (or text "") nil (smoke-out "screen.txt"))
             (smoke-log "画面:\n%s" text)
             (smoke-check (and text (string-match-p "hello from emacs" text)) "キーで打った文字が画面に出た")))
         (smoke-next))
       (lambda ()
         ;; 5. 日本語と eaw を出す（送る文字列）
         (with-current-buffer smoke-buf
           (embterm-module-send-string embterm--term "printf '日本語 ①★※ ─│┌┐\\n'\r"))
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (let ((text (embterm-module-text embterm--term)))
             (smoke-check (and text (string-match-p "日本語 ①★※" text)) "日本語と eaw が画面に出た")
             (embterm-module-screenshot embterm--term (smoke-out "pane.png") nil)))
         (smoke-next))
       (lambda ()
         ;; 6. 画面の保存（知らせで受け取る）
         (let ((s (smoke-state)))
           (smoke-log "保存のあとの状態: %S" s))
         (smoke-check (file-exists-p (smoke-out "pane.png")) "ペインの画面を PNG に保存した")
         (smoke-next))
       (lambda ()
         ;; 7. 分割して 2 つ目のペイン
         (with-current-buffer smoke-buf
           (split-window-below)
           (redisplay t))
         (smoke-next))
       (lambda ()
         (let* ((s (smoke-state))
                (panes (plist-get s :panes)))
           (smoke-log "分割のあとのペイン: %S" panes)
           (smoke-check (>= (length panes) 2) "ペインが 2 つになった"))
         (smoke-next))
       (lambda ()
         ;; 8. 閉じる
         (with-current-buffer smoke-buf
           (let ((kill-buffer-query-functions nil))
             (kill-buffer smoke-buf)))
         (smoke-check (not (buffer-live-p smoke-buf)) "バッファを消した")
         (smoke-next))))

(run-at-time 1.0 nil #'smoke-next)
;; 時間の上限
(run-at-time 60 nil (lambda () (smoke-check nil "時間切れ") (smoke-finish)))
