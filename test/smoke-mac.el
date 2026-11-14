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
(defvar smoke-ime-before nil)
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
         ;; 7. マウスのドラッグで選択（T-127）。2 行目（hello from emacs）をなぞる
         (with-current-buffer smoke-buf
           (let* ((st (smoke-state))
                  (rows (plist-get st :rows))
                  (win (get-buffer-window smoke-buf))
                  (edges (window-inside-pixel-edges win))
                  (h (- (nth 3 edges) (nth 1 edges)))
                  (row-h (/ (float h) (max rows 1)))
                  (y (round (* row-h 1.5)))
                  (pane (car (plist-get st :panes))))
             (embterm-module-mouse embterm--term (car pane) "down" 2 y 1 "")
             (embterm-module-mouse embterm--term (car pane) "drag" 300 y 1 "")
             (embterm-module-mouse embterm--term (car pane) "up" 300 y 1 "")
             (let ((sel (embterm-module-selection embterm--term)))
               (smoke-log "選択: %S" sel)
               (smoke-check (and sel (string-match-p "hello from emacs" sel))
                            "マウスのドラッグで選択できた"))))
         (smoke-next))
       (lambda ()
         ;; 8. IME の変換中の文字列（T-127）
         (with-current-buffer smoke-buf
           (embterm-module-clear-selection embterm--term)
           (embterm-module-set-preedit embterm--term "にほんご")
           (redisplay t))
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (embterm-module-screenshot embterm--term (smoke-out "preedit.png") nil))
         (smoke-next))
       (lambda ()
         (smoke-check (file-exists-p (smoke-out "preedit.png")) "変換中の文字列を描いた画面を保存した")
         (with-current-buffer smoke-buf
           ;; NS の経路（advice）も通ることを確かめる
           (setq-local ns-working-text "かんじ")
           (if (fboundp 'ns-put-working-text)
               (progn (ns-put-working-text)
                      (smoke-check t "ns-put-working-text を端末が受け取った"))
             (smoke-check nil "ns-put-working-text が無い"))
           (embterm-module-set-preedit embterm--term ""))
         (smoke-next))
       (lambda ()
         ;; 8c. 置き場所が変わってもアニメーションが入らない（T-132）。
         ;; ウィンドウの高さを変えた直後に見る（暗黙のアニメーションは 0.25 秒で終わるので後では分からない）
         (with-current-buffer smoke-buf
           (let ((win (get-buffer-window smoke-buf)))
             (window-resize win -3)
             (redisplay t)
             (let ((i (embterm-module-debug-info embterm--term)))
               (smoke-log "高さを変えた直後の debug-info: %S" i)
               (smoke-check (eql 0 (plist-get i :layer-animations))
                            "置き場所を変えてもレイヤーのアニメーションが入らない（%s 本）"
                            (plist-get i :layer-animations)))
             (window-resize win 3)
             (redisplay t)))
         (smoke-next))
       (lambda ()
         ;; 9. 分割して 2 つ目のペイン
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
         ;; 10. IME の候補ウィンドウの位置（T-129）。端末のカーソルを追うこと
         (with-current-buffer smoke-buf
           (let* ((before (embterm-module-ime-rect embterm--term)))
             (smoke-log "IME の矩形（打つ前）: %S" before)
             (smoke-check (nth 4 before) "firstRectForCharacterRange: の差し替えが入っている")
             (setq smoke-ime-before before)
             ;; カーソルを右へ動かす（空白を 20 個）
             (embterm-module-send-string embterm--term (make-string 20 ?x))))
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (let ((after (embterm-module-ime-rect embterm--term)))
             (smoke-log "IME の矩形（打った後）: %S" after)
             (smoke-check (> (nth 0 after) (nth 0 smoke-ime-before))
                          "カーソルが右へ動いたら IME の位置も右へ動いた（%.1f → %.1f）"
                          (nth 0 smoke-ime-before) (nth 0 after))
             (smoke-check (> (nth 3 after) 0) "矩形の高さが 0 でない（%.1f）" (nth 3 after))
             ;; 端末を選んでいない状態では元の動き（Emacs のポイント）に戻る
             (embterm-module-send-key embterm--term "c" "C")
             (embterm-module-select embterm--term nil)))
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (let ((off (embterm-module-ime-rect embterm--term)))
             (smoke-log "IME の矩形（端末を選んでいない）: %S" off)
             (smoke-check (not (equal (seq-take off 2) (seq-take smoke-ime-before 2)))
                          "端末を選んでいないときは元の位置に戻る")
             (embterm-module-select embterm--term 1)))
         (smoke-next))
       (lambda ()
         ;; 11. カーソルの点滅（T-127。描画スレッドのタイマー）。既定は点滅しないので有効にする
         (customize-set-variable 'embterm-cursor-blink t)
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (let ((info (embterm-module-debug-info embterm--term)))
             (smoke-log "debug-info: %S" info)
             (smoke-check (> (or (plist-get info :blink-ticks) 0) 0) "カーソルの点滅が動いている")
             (smoke-check (> (or (plist-get info :full-draws) 0) 0) "描画の回数が数えられている")))
         (smoke-next))
       (lambda ()
         ;; 12. copy モード（C-c C-k。スクロールバックをバッファに写す）
         (with-current-buffer smoke-buf
           (embterm-copy-mode))
         (smoke-next))
       (lambda ()
         (with-current-buffer smoke-buf
           (let ((text (buffer-string)))
             (smoke-log "copy モードのバッファ（先頭 200 字）: %S" (substring text 0 (min 200 (length text))))
             (smoke-check (string-match-p "hello from emacs" text) "copy モードに画面のテキストが入った")
             (smoke-check (not (string-match-p "写しています" text)) "copy モードの準備中の表示が消えた")))
         (with-current-buffer smoke-buf (embterm-semi-char-mode))
         (smoke-next))
       (lambda ()
         ;; 13. 閉じる
         (with-current-buffer smoke-buf
           (let ((kill-buffer-query-functions nil))
             (kill-buffer smoke-buf)))
         (smoke-check (not (buffer-live-p smoke-buf)) "バッファを消した")
         (smoke-next))))

(run-at-time 1.0 nil #'smoke-next)
;; 時間の上限
(run-at-time 60 nil (lambda () (smoke-check nil "時間切れ") (smoke-finish)))
