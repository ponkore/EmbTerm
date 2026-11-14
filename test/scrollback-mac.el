;;; scrollback-mac.el --- スクロールバックの自動確認（T-135） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で）:
;;
;;   cargo xtask dist
;;   EMBTERM_ROOT=$PWD emacs -Q -l $PWD/test/scrollback-mac.el
;;
;; 見ているのは 3 つ:
;;   1. 遡ったときに**スクロールバックの行が出る**か（T-135 の直したところ）
;;   2. ホイールの向き（`wheel-up' = 遡る。Windows の `wheel_pane' と同じ）
;;   3. 遡った画面が**実際に描けている**か（PNG の大きさで見る。空の画面との比）
;;
;; 合否は終了コード（0 = OK）。結果は target/t135/ に書く。

(require 'cl-lib)
(require 'seq)

(defconst sb-root
  (or (getenv "EMBTERM_ROOT")
      (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst sb-out (expand-file-name "target/t135/" sb-root))
(defconst sb-log-file (expand-file-name "scrollback-mac.log" sb-out))
(defvar sb-failures nil)

(make-directory sb-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p sb-log-file)
  (delete-file sb-log-file))

(defun sb-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil sb-log-file t 'silent)))

(defun sb-out (name) (expand-file-name name sb-out))

(defun sb-check (ok fmt &rest args)
  (let ((msg (apply #'format fmt args)))
    (sb-log "%s %s" (if ok "OK" "NG") msg)
    (unless ok (push msg sb-failures))
    ok))

(setq embterm-module-directory (expand-file-name "target/dist" sb-root))
(load (expand-file-name "target/dist/embterm.el" sb-root) nil t)
(setq embterm-shell '("/bin/sh"))
(setq embterm-poll-interval 0.05)

(defvar sb-buf nil)
(defvar sb-steps nil)
(defvar sb-got-before nil)

(defun sb-state ()
  (when (buffer-live-p sb-buf)
    (with-current-buffer sb-buf
      (and embterm--term (embterm-module-poll embterm--term nil)))))

(defun sb-pane-id ()
  (car (car (plist-get (sb-state) :panes))))

(defun sb-text ()
  "ペインの見えている行（`embterm-module-text' は端末のビューポートなので、遡っても変わらない）。"
  (with-current-buffer sb-buf (or (embterm-module-pane-text embterm--term (sb-pane-id)) "")))

(defun sb-send (s)
  (with-current-buffer sb-buf (embterm-module-send-string embterm--term s)))

(defun sb-scroll (kind &optional n)
  (with-current-buffer sb-buf
    (embterm-module-scroll embterm--term (sb-pane-id) kind n)))

(defun sb-shot (name)
  (with-current-buffer sb-buf
    (embterm-module-screenshot embterm--term (sb-out name) nil)))

(defun sb-wheel (dir n)
  "DIR（`wheel-up' / `wheel-down'）のホイールを N 回、Emacs の経路（`embterm-wheel'）で送る。"
  (let* ((win (get-buffer-window sb-buf))
         (ev (list dir (list win 1 '(0 . 0) 0) 1)))
    (dotimes (_ n) (embterm-wheel ev))))

(defun sb-gz-size (name)
  "PNG を gzip して大きさを見る（圧縮しない PNG なのでファイルの大きさは中身によらず同じ。
空の画面はよく縮み、文字が並んだ画面は縮まない）。"
  (let ((default-directory sb-out))
    (with-temp-buffer
      (set-buffer-multibyte nil)
      (call-process "gzip" (expand-file-name name sb-out) t nil "-c" "-9")
      (buffer-size))))

(defun sb-line-visible-p (text n)
  (string-match-p (format "^%d$" n) text))

(defun sb-last-got (text)
  "子プロセス（test/mouse-child.py）が最後に受け取ったものの行。"
  (car (last (seq-filter (lambda (l) (string-prefix-p "GOT " l)) (split-string text "\n")))))

(defun sb-child (mode)
  (sb-send (format "python3 %s %s\r" (expand-file-name "test/mouse-child.py" sb-root) mode)))

(defun sb-next ()
  (if-let* ((step (pop sb-steps)))
      (run-at-time 0.9 nil (lambda () (condition-case e (funcall step)
                                        (error (sb-check nil "手順で error: %S" e) (sb-finish)))))
    (sb-finish)))

(defun sb-finish ()
  (sb-log "終わり（NG %d 件）" (length sb-failures))
  (kill-emacs (if sb-failures 1 0)))

(setq sb-steps
      (list
       (lambda ()
         ;; 1. 端末を開く
         (embterm--load-module)
         (embterm-module-set-log (sb-out "module.log"))
         (setq sb-buf (embterm))
         (sb-check (buffer-live-p sb-buf) "端末のバッファができた")
         (redisplay t)
         (sb-next))
       (lambda ()
         (let ((s (sb-state)))
           (sb-log "状態: %S" s)
           (sb-check (plist-get s :started) "子プロセスが起動した")
           (sb-check (> (plist-get s :rows) 10) "%s 桁 x %s 行" (plist-get s :cols) (plist-get s :rows)))
         ;; 空の画面を撮っておく（遡った画面が「描けている」かを比べる基準）
         (sb-send "clear\r")
         (sb-next))
       (lambda ()
         (sb-shot "blank.png")
         (sb-next))
       (lambda ()
         (sb-check (file-exists-p (sb-out "blank.png")) "空の画面を撮った（%d バイト）"
                   (or (file-attribute-size (file-attributes (sb-out "blank.png"))) 0))
         ;; 2. 画面よりずっと多い行を出す
         (sb-send "seq 1 200\r")
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (write-region text nil (sb-out "bottom.txt"))
           (sb-check (sb-line-visible-p text 200) "いちばん下では 200 行目が見えている"))
         (sb-shot "bottom.png")
         (sb-next))
       (lambda ()
         ;; 3. 10 行遡る（`lines' は行の番号を動かす向きなので負が遡る）
         (sb-scroll "lines" -10)
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (write-region text nil (sb-out "scrolled.txt"))
           (sb-log "10 行遡った画面:\n%s" text)
           (sb-check (not (sb-line-visible-p text 200)) "10 行遡ったら 200 行目は見えない")
           (sb-check (sb-line-visible-p text 185) "10 行遡ったら 185 行目が見えている"))
         (sb-shot "scrolled.png")
         (sb-next))
       (lambda ()
         ;; 4. いちばん上まで遡る（画面の行数よりずっと深い。直す前はここが真っ暗だった）
         (sb-scroll "top")
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (write-region text nil (sb-out "top.txt"))
           (sb-log "いちばん上まで遡った画面:\n%s" text)
           (sb-check (string-match-p "seq 1 200" text) "いちばん上ではコマンドを打った行が見えている")
           (sb-check (sb-line-visible-p text 1) "いちばん上では 1 行目が見えている"))
         (sb-shot "top.png")
         (sb-next))
       (lambda ()
         ;; 5. 遡った画面が実際に描けているか（空の画面より PNG がずっと大きい = 文字が出ている）
         (let ((blank (sb-gz-size "blank.png"))
               (bottom (sb-gz-size "bottom.png"))
               (top (sb-gz-size "top.png")))
           (sb-log "gzip した PNG の大きさ: blank=%d bottom=%d top=%d" blank bottom top)
           (sb-check (> top (* 1.5 blank))
                     "いちばん上まで遡った画面に文字が描けている（gzip %d > 空の画面 %d の 1.5 倍。直す前は %d < %d で真っ暗だった）"
                     top blank 3413 5057))
         (sb-scroll "bottom")
         (sb-next))
       (lambda ()
         (sb-check (sb-line-visible-p (sb-text) 200) "いちばん下へ戻ると 200 行目が見える")
         ;; 6. ホイールの向き（Emacs の経路。`wheel-up' = 遡る）
         (sb-wheel 'wheel-up 4)
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (sb-check (not (sb-line-visible-p text 200)) "wheel-up で遡った（200 行目が見えない）")
           (sb-check (sb-line-visible-p text 185) "wheel-up のあと 185 行目が見えている"))
         (sb-wheel 'wheel-down 4)
         (sb-next))
       (lambda ()
         (sb-check (sb-line-visible-p (sb-text) 200) "wheel-down でいちばん下へ戻った")
         ;; 7. マウスを見るアプリ（claude / vim / yazi）の上では、ホイールは**アプリへ報告**する（T-136）
         (sb-child "mouse")
         (sb-next))
       (lambda ()
         (sb-check (string-match-p "READY mouse" (sb-text)) "マウスを見る子プロセスが動いた")
         (sb-wheel 'wheel-up 1)
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (sb-log "wheel-up をアプリへ: %s" (sb-last-got text))
           ;; SGR（DEC 1006）のホイールは上が 64、下が 65
           (sb-check (string-match-p "GOT .*\\[<64;" text) "wheel-up がマウスの報告としてアプリに届いた"))
         (sb-wheel 'wheel-down 1)
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (sb-log "wheel-down をアプリへ: %s" (sb-last-got text))
           (sb-check (string-match-p "GOT .*\\[<65;" text) "wheel-down がマウスの報告としてアプリに届いた")
           ;; 8. Shift を足すと、アプリに見せずに端末のスクロールバックを動かす
           (setq sb-got-before (sb-last-got text)))
         (sb-wheel 'S-wheel-up 1)
         (sb-next))
       (lambda ()
         (sb-scroll "bottom")
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (sb-check (equal (sb-last-got text) sb-got-before)
                     "S-wheel-up はアプリに届かない（最後の報告が %s のまま）" sb-got-before))
         ;; 子プロセスを終わらせて、代替画面のアプリを動かす
         (sb-send "q")
         (sb-next))
       (lambda ()
         (sb-child "alt")
         (sb-next))
       (lambda ()
         (sb-check (string-match-p "READY alt" (sb-text)) "代替画面の子プロセスが動いた")
         (sb-wheel 'wheel-up 1)
         (sb-next))
       (lambda ()
         (let ((text (sb-text)))
           (sb-log "代替画面でのホイール: %s" (sb-last-got text))
           ;; 代替画面にスクロールバックは無いので、DEC 1007 で矢印にする（Ghostty と同じ）
           (sb-check (string-match-p "GOT .*x1b\\[A" text) "代替画面では wheel-up が上矢印になってアプリに届いた"))
         (sb-send "q")
         (sb-next))
       (lambda ()
         (with-current-buffer sb-buf
           (let ((kill-buffer-query-functions nil))
             (kill-buffer sb-buf)))
         (sb-check (not (buffer-live-p sb-buf)) "バッファを消した")
         (sb-next))))

(run-at-time 1.0 nil #'sb-next)
(run-at-time 90 nil (lambda () (sb-check nil "時間切れ") (sb-finish)))
