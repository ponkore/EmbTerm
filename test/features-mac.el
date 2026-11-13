;;; features-mac.el --- macOS の機能の自動確認（T-128） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist && cargo xtask test-emacs features
;;
;; smoke-mac.el（T-126 / T-127 の基本）に対して、こちらは設定と統合を確かめる:
;;   - リサイズ（ウィンドウの大きさ → 桁数・行数 → 子プロセス）
;;   - フォントの変更（セルの大きさと桁数が変わる）
;;   - 色（`embterm-follow-theme' で Emacs のテーマに追従する）
;;   - 環境変数（`setenv' が子に届く。T-109）
;;   - スクロール（スクロールバックを遡って戻る）
;;   - シェル統合（OSC 133 のプロンプトの印、OSC 51;E での関数の呼び出し。T-118 / T-119）
;;   - ハイパーリンク（OSC 8）
;; 合否は終了コード。結果は target/t128/features-mac.log

(require 'cl-lib)
(require 'seq)

(defconst f-root (or (getenv "EMBTERM_ROOT")
                     (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst f-out (expand-file-name "target/t128/" f-root))
(defconst f-log (expand-file-name "features-mac.log" f-out))
(defvar f-failures nil)

(make-directory f-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p f-log) (delete-file f-log))

(defun f-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))
                nil f-log t 'silent))

(defun f-check (ok fmt &rest args)
  (let ((msg (apply #'format fmt args)))
    (f-log "%s %s" (if ok "OK" "NG") msg)
    (unless ok (push msg f-failures))
    ok))

(setq embterm-module-directory (expand-file-name "target/dist" f-root))
(load (expand-file-name "target/dist/embterm.el" f-root) nil t)
(setq embterm-shell '("/bin/sh"))
(setq embterm-poll-interval 0.05)

(defvar f-buf nil)
(defvar f-steps nil)

(defun f-state ()
  (when (buffer-live-p f-buf)
    (with-current-buffer f-buf (and embterm--term (embterm-module-poll embterm--term nil)))))

(defun f-text ()
  (when (buffer-live-p f-buf)
    (with-current-buffer f-buf (or (ignore-errors (embterm-module-text embterm--term)) ""))))

(defun f-send (s)
  (with-current-buffer f-buf (embterm-module-send-string embterm--term s)))

(defun f-next (&optional delay)
  (if-let* ((step (pop f-steps)))
      (run-at-time (or delay 0.9) nil
                   (lambda () (condition-case e (funcall step)
                                (error (f-check nil "手順で error: %S" e) (f-finish)))))
    (f-finish)))

(defun f-finish ()
  (f-log "終わり（NG %d 件）" (length f-failures))
  (kill-emacs (if f-failures 1 0)))

(setq f-steps
      (list
       (lambda ()
         (embterm--load-module)
         (embterm-module-set-log (expand-file-name "features-module.log" f-out))
         ;; 環境変数（T-109）: 子に届くこと
         (setenv "EMBTERM_TEST_VAR" "ごきげんよう")
         (setq f-buf (embterm))
         (redisplay t)
         (f-next 1.2))
       (lambda ()
         (f-send "printf 'VAR=[%s]\\n' \"$EMBTERM_TEST_VAR\"\r")
         (f-next 1.2))
       (lambda ()
         (f-check (string-match-p "VAR=\\[ごきげんよう\\]" (f-text)) "setenv が子プロセスに届いた（T-109）")
         ;; TERM も確かめる
         (f-send "printf 'TERM=[%s] COLORTERM=[%s] PROG=[%s]\\n' \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\"\r")
         (f-next 1.2))
       (lambda ()
         (let ((text (f-text)))
           (f-check (string-match-p "TERM=\\[xterm-256color\\]" text) "TERM が xterm-256color")
           (f-check (string-match-p "PROG=\\[EmbTerm\\]" text) "TERM_PROGRAM が EmbTerm"))
         (f-next))
       (lambda ()
         ;; リサイズ（ウィンドウを狭める → 桁数が減る → 子に伝わる）
         (let ((before (plist-get (f-state) :cols)))
           (f-log "リサイズ前の桁数: %s" before)
           ;; ウィンドウが 1 つだと `shrink-window-horizontally' は効かないので、フレームを狭める
           (set-frame-width (selected-frame) (- (frame-width) 20))
           (redisplay t)
           (setq f-cols-before before))
         (f-next 1.2))
       (lambda ()
         (let ((after (plist-get (f-state) :cols)))
           (f-log "リサイズ後の桁数: %s" after)
           (f-check (< after f-cols-before) "ウィンドウを狭めたら桁数が減った（%s → %s）" f-cols-before after)
           (f-send "stty size\r"))
         (f-next 1.2))
       (lambda ()
         (let* ((st (f-state))
                (want (format "%d %d" (plist-get st :rows) (plist-get st :cols))))
           (f-check (string-match-p (regexp-quote want) (f-text)) "子プロセスにも新しい大きさが伝わった（stty size = %s）" want))
         (f-next))
       (lambda ()
         ;; フォントを変える（セルの大きさと桁数が変わる）
         (let ((cell (plist-get (f-state) :cols)))
           (setq f-cols-before cell))
         (customize-set-variable 'embterm-font-size 20.0)
         (redisplay t)
         (f-next 1.2))
       (lambda ()
         (let ((after (plist-get (f-state) :cols)))
           (f-check (< after f-cols-before) "フォントを大きくしたら桁数が減った（%s → %s）" f-cols-before after))
         (customize-set-variable 'embterm-font-size nil)
         (f-next 1.0))
       (lambda ()
         ;; 色（Emacs のテーマに追従する。T-034）
         (with-current-buffer f-buf
           (let ((info (embterm-module-debug-info embterm--term)))
             (f-log "色: %S" (list (plist-get info :foreground) (plist-get info :background)))
             (f-check (equal (color-values (or (plist-get info :background) "#000000"))
                             (color-values (or (face-background 'default nil t) "#ffffff")))
                      "端末の背景が Emacs の `default' と同じ（%s / %s）"
                      (plist-get info :background) (face-background 'default nil t))))
         (f-next))
       (lambda ()
         ;; スクロール（スクロールバックを遡って戻る）
         (f-send "seq 1 200\r")
         (f-next 1.5))
       (lambda ()
         (with-current-buffer f-buf
           (let ((pane (car (plist-get (f-state) :panes))))
             (embterm-module-scroll embterm--term (car pane) "lines" -30)))
         (f-next 0.8))
       (lambda ()
         (let* ((st (f-state))
                (pane (car (plist-get st :panes))))
           (f-check (nth 4 pane) "遡っている印（scrolled）が立った")
           (with-current-buffer f-buf
             (embterm-module-scroll embterm--term (car pane) "bottom" nil)))
         (f-next 0.8))
       (lambda ()
         (let ((pane (car (plist-get (f-state) :panes))))
           (f-check (not (nth 4 pane)) "いちばん下へ戻った"))
         (f-next))
       (lambda ()
         ;; シェル統合（T-118 / T-119）: etc/embterm.bash を読み込んで OSC 133 と OSC 51;E を使う
         (f-send (format ". %s\r" (shell-quote-argument (expand-file-name "etc/embterm.bash" f-root))))
         (f-next 1.0))
       (lambda ()
         (f-send "embterm_cmd message 'シェルから呼んだ'\r")
         (f-next 1.2))
       (lambda ()
         ;; message は *Messages* に残る
         (with-current-buffer "*Messages*"
           (f-check (string-match-p "シェルから呼んだ" (buffer-string)) "OSC 51;E でシェルから Emacs の関数を呼べた（T-118）"))
         (f-next))
       (lambda ()
         ;; OSC 8 のハイパーリンク
         (f-send "printf '\\033]8;;https://example.com/embterm\\033\\\\link\\033]8;;\\033\\\\\\n'\r")
         (f-next 1.2))
       (lambda ()
         (with-current-buffer f-buf
           (let* ((st (f-state))
                  (pane (car (plist-get st :panes)))
                  (info (embterm-module-debug-info embterm--term))
                  ;; セルの大きさはデバイス画素なので、論理ポイントに直す（拡大率 2）
                  (cell-h (max 1 (/ (cdr (plist-get info :cell)) 2)))
                  (rows (plist-get st :rows))
                  (uri nil))
             (dotimes (y rows)
               (unless uri
                 (let ((found (embterm-module-link-at embterm--term (car pane) 4 (+ 2 (* y cell-h)))))
                   (when found (setq uri found)))))
             (f-log "リンク: %S" uri)
             (f-check (equal uri "https://example.com/embterm") "OSC 8 のリンクを画素の位置から引けた")))
         (f-next))
       (lambda ()
         (let ((kill-buffer-query-functions nil))
           (when (buffer-live-p f-buf) (kill-buffer f-buf)))
         (f-check (not (buffer-live-p f-buf)) "バッファを消した")
         (f-next 0.8))))

(defvar f-cols-before nil)
(run-at-time 1.0 nil #'f-next)
(run-at-time 120 nil (lambda () (f-check nil "時間切れ") (f-finish)))
