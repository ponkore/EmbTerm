;;; perf-mac.el --- macOS の性能の測定（T-128。Windows の perf.el に当たる） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   cargo xtask test-emacs perf        （または）
;;   EMBTERM_ROOT=$PWD PERF_TAG=run emacs -Q -l $PWD/test/perf-mac.el
;;
;; 測るもの（結果は target/t128/perf-TAG.log）:
;;   - 何もしていないときと、大量の出力（10 MB の `cat`）の最中の Emacs の 10 ms タイマーの間隔の分布
;;     → **Emacs が固まらないこと**（この移植のいちばんの売り）
;;   - module の測定（読みのスレッドのロックと vt_write、描画の 1 枚あたり、入力から表示までの遅れ）
;;   - 画面いっぱいの色付きの文字（描画の重い場合）
;;   - 大きな PNG（4000x4000）のデコード
;;   - 端末を 15 個開いたときのメモリ・スレッド数
;; 待ちはすべて `run-at-time' で作る（`sit-for' は使わない）

(require 'cl-lib)
(require 'seq)

(defconst perf-root (or (getenv "EMBTERM_ROOT")
                        (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst perf-out (expand-file-name "target/t128/" perf-root))
(defconst perf-tag (or (getenv "PERF_TAG") "run"))
(defconst perf-log-file (expand-file-name (format "perf-%s.log" perf-tag) perf-out))
(defconst perf-data (expand-file-name "data.txt" perf-out))
(defconst perf-timeout 420)
(defvar perf-failures nil)

(make-directory perf-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p perf-log-file) (delete-file perf-log-file))

(defun perf-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil perf-log-file t 'silent)))

(defun perf-check (ok fmt &rest args)
  (let ((msg (apply #'format fmt args)))
    (perf-log "%s %s" (if ok "OK" "NG") msg)
    (unless ok (push msg perf-failures))))

;; module と embterm.el は配布物から
(setq embterm-module-directory (expand-file-name "target/dist" perf-root))
(load (expand-file-name "target/dist/embterm.el" perf-root) nil t)
(setq embterm-shell '("/bin/sh"))
(setq embterm-poll-interval 0.05)

;;;; Emacs のタイマーの間隔（固まっていないか）

(defvar perf-ticks nil)
(defvar perf-tick-last nil)
(defvar perf-tick-timer nil)

(defun perf-tick ()
  (let ((now (float-time)))
    (when perf-tick-last (push (* 1000 (- now perf-tick-last)) perf-ticks))
    (setq perf-tick-last now)))

(defun perf-tick-start ()
  (setq perf-ticks nil perf-tick-last nil
        perf-tick-timer (run-at-time 0 0.01 #'perf-tick)))

(defun perf-quantile (sorted q)
  (nth (round (* q (1- (length sorted)))) sorted))

(defun perf-tick-stop (label &optional limit)
  (when perf-tick-timer (cancel-timer perf-tick-timer))
  (let ((v (sort (copy-sequence perf-ticks) #'<)))
    (if (null v)
        (perf-log "タイマー（%s）: 無し" label)
      (let ((p99 (perf-quantile v 0.99)) (max (car (last v))))
        (perf-log "タイマー（%s）: n=%d p50=%.1f p90=%.1f p99=%.1f max=%.1f ms、50 ms 超え %d 回"
                  label (length v) (perf-quantile v 0.5) (perf-quantile v 0.9) p99 max
                  (seq-count (lambda (x) (> x 50)) v))
        (when limit
          (perf-check (< p99 limit) "%s の p99 が %.0f ms 未満（%.1f ms）" label limit p99))))))

;;;; 手順

(defvar perf-buf nil)
(defvar perf-many nil)

(defun perf-report (label &optional reset)
  (when (buffer-live-p perf-buf)
    (with-current-buffer perf-buf
      (let ((r (embterm-module-perf embterm--term reset)))
        (perf-log "module（%s）:\n%s" label
                  (mapconcat (lambda (l) (concat "    " l)) (split-string (or r "（応答なし）") "\n" t) "\n"))))))

(defun perf-proc (label)
  (let ((out (with-output-to-string
               (call-process "sh" nil standard-output nil
                             (expand-file-name "test/proc-stats.sh" perf-root)
                             (number-to-string (emacs-pid))))))
    (perf-log "プロセス（%s）: %s" label (string-trim out))))

(defun perf-send (string)
  (with-current-buffer perf-buf (embterm-module-send-string embterm--term string)))

(defvar perf-cat-start nil)
(defvar perf-cat-done nil)

(defun perf-watch-done ()
  "画面に `__CAT_DONE__' が出たら時刻を覚える（0.1 秒ごとに見る）。"
  (run-at-time
   0.1 nil
   (lambda ()
     (cond
      (perf-cat-done)
      ((and (buffer-live-p perf-buf)
            (with-current-buffer perf-buf
              (let ((text (ignore-errors (embterm-module-text embterm--term))))
                (and text (string-match-p "__CAT_DONE__" text)))))
       (setq perf-cat-done (float-time)))
      ((> (- (float-time) perf-cat-start) 20) nil)
      (t (perf-watch-done))))))

(defvar perf-steps nil)

(defun perf-next (&optional delay)
  (if-let* ((step (pop perf-steps)))
      (run-at-time (or delay 0.8) nil
                   (lambda () (condition-case e (funcall step)
                                (error (perf-check nil "手順で error: %S" e) (perf-finish)))))
    (perf-finish)))

(defun perf-finish ()
  (perf-log "終わり（NG %d 件）" (length perf-failures))
  (kill-emacs (if perf-failures 1 0)))

(setq perf-steps
      (list
       (lambda ()
         (embterm--load-module)
         (embterm-module-set-log (expand-file-name (format "module-%s.log" perf-tag) perf-out))
         (perf-log "module %s / Emacs %s" embterm--module-loaded emacs-version)
         (setq perf-buf (embterm))
         (redisplay t)
         (perf-next 1.2))
       (lambda ()
         ;; 1. 何もしていないとき
         (perf-tick-start)
         (perf-next 3.0))
       (lambda ()
         (perf-tick-stop "何もしていないとき" 50)
         (perf-proc "端末 1 個")
         ;; 10 MB・20 万行のデータを作る（初回だけ）
         (unless (file-exists-p perf-data)
           (perf-log "データを作る: %s" perf-data)
           (call-process "sh" nil nil nil "-c"
                         (format "awk 'BEGIN{for(i=0;i<200000;i++) printf \"%%06d 0123456789abcdefghijklmnopqrstuvwxyz0123456789\\n\", i}' > %s"
                                 (shell-quote-argument perf-data))))
         (perf-log "データ: %s バイト" (file-attribute-size (file-attributes perf-data)))
         (perf-next))
       (lambda ()
         ;; 2. 大量の出力（cat）の最中
         (perf-report "cat の前" t)
         (perf-tick-start)
         (perf-next 0.3))
       (lambda ()
         ;; 画面に出しながらの cat（端末の処理が律速になる）。終わりの印が出るまでの時間も測る
         ;; 印は入力の反響に当たらないよう分けて書く
         (setq perf-cat-start (float-time) perf-cat-done nil)
         (perf-send (format "/bin/cat %s; echo \"__CAT\"\"_DONE__\"\r" (shell-quote-argument perf-data)))
         (perf-watch-done)
         (perf-next 16.0))
       (lambda ()
         (perf-tick-stop "10 MB の cat の最中" 60)
         (perf-log "cat が終わるまで: %s"
                   (if perf-cat-done (format "%.2f 秒" (- perf-cat-done perf-cat-start)) "（16 秒では終わらなかった）"))
         (perf-report "cat のあと")
         (perf-proc "cat のあと")
         (perf-next))
       (lambda ()
         ;; 3. 画面いっぱいの色付きの文字
         (perf-report "色の前" t)
         (perf-tick-start)
         (perf-send (format "sh %s\r" (shell-quote-argument (expand-file-name "test/perf-colors.sh" perf-root))))
         (perf-next 8.0))
       (lambda ()
         (perf-tick-stop "色付きの画面の最中" 60)
         (perf-report "色のあと")
         (perf-next))
       (lambda ()
         ;; 4. 大きな PNG（4000x4000）
         (perf-report "画像の前" t)
         (perf-send (format "python3 -I %s big --no-wait\r"
                            (shell-quote-argument (expand-file-name "test/kitty-graphics.py" perf-root))))
         (perf-next 6.0))
       (lambda ()
         (perf-report "大きな画像のあと")
         (perf-proc "画像のあと")
         (perf-next))
       (lambda ()
         ;; 5. 端末を 15 個
         (setq perf-many (cl-loop repeat 14 collect (embterm-new)))
         (redisplay t)
         (perf-log "端末の数: %d" (1+ (length perf-many)))
         (perf-next 3.0))
       (lambda ()
         (perf-tick-start)
         (perf-next 3.0))
       (lambda ()
         (perf-tick-stop "端末 15 個で何もしていないとき" 50)
         (perf-proc "端末 15 個")
         (perf-check (> (length (seq-filter #'buffer-live-p perf-many)) 10) "端末を 15 個開けた")
         (perf-next))
       (lambda ()
         ;; 後始末
         (let ((kill-buffer-query-functions nil))
           (dolist (b (cons perf-buf perf-many))
             (when (buffer-live-p b) (kill-buffer b))))
         (perf-log "閉じた")
         (perf-next 1.5))))

(run-at-time 1.0 nil #'perf-next)
(run-at-time perf-timeout nil (lambda () (perf-check nil "時間切れ") (perf-finish)))
