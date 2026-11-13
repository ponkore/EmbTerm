;;; perf-ghostel.el --- ghostel と EmbTerm の比べ（T-122 / T-128） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   EMBTERM_ROOT=$PWD emacs -Q -l $PWD/test/perf-ghostel.el
;;
;; 利用者の ~/.emacs.d に入っている ghostel（lisp は straight のリポジトリ、module は ~/.emacs.d/ghostel/）を
;; `emacs -Q' に読み込み、**EmbTerm の perf-mac.el と同じ測り方**で比べる:
;;   - 何もしていないときと、10 MB の `cat' の最中の Emacs の 10 ms タイマーの間隔
;;   - `cat' が終わるまでの時間
;; ghostel が無ければ何もせずに終わる（NG にしない）。ghostel の設定は読まない（`emacs -Q' + 最小限）。
;; 結果は target/t128/ghostel.log

(require 'cl-lib)
(require 'seq)

(defconst g-root (or (getenv "EMBTERM_ROOT")
                     (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst g-out (expand-file-name "target/t128/" g-root))
(defconst g-log (expand-file-name "ghostel.log" g-out))
(defconst g-data (expand-file-name "data.txt" g-out))
(defconst g-lisp (or (getenv "GHOSTEL_LISP") (expand-file-name "~/.emacs.d/straight/repos/ghostel/lisp")))
(defconst g-module (or (getenv "GHOSTEL_MODULE") (expand-file-name "~/.emacs.d/ghostel")))

(make-directory g-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p g-log) (delete-file g-log))

(defun g-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil g-log t 'silent)))

(defvar g-ticks nil)
(defvar g-tick-last nil)
(defvar g-tick-timer nil)

(defun g-tick ()
  (let ((now (float-time)))
    (when g-tick-last (push (* 1000 (- now g-tick-last)) g-ticks))
    (setq g-tick-last now)))

(defun g-tick-start ()
  (setq g-ticks nil g-tick-last nil g-tick-timer (run-at-time 0 0.01 #'g-tick)))

(defun g-quantile (sorted q) (nth (round (* q (1- (length sorted)))) sorted))

(defun g-tick-stop (label)
  (when g-tick-timer (cancel-timer g-tick-timer))
  (let ((v (sort (copy-sequence g-ticks) #'<)))
    (if (null v)
        (g-log "タイマー（%s）: 無し" label)
      (g-log "タイマー（%s）: n=%d p50=%.1f p90=%.1f p99=%.1f max=%.1f ms、50 ms 超え %d 回、100 ms 超え %d 回"
             label (length v) (g-quantile v 0.5) (g-quantile v 0.9) (g-quantile v 0.99) (car (last v))
             (seq-count (lambda (x) (> x 50)) v) (seq-count (lambda (x) (> x 100)) v)))))

(defvar g-buf nil)
(defvar g-steps nil)
(defvar g-cat-start nil)
(defvar g-cat-done nil)

(defun g-watch-done ()
  "画面に `__CAT_DONE__' が出たら時刻を覚える（0.1 秒ごとに見る）。"
  (run-at-time
   0.1 nil
   (lambda ()
     (cond
      (g-cat-done)
      ((and (buffer-live-p g-buf)
            (with-current-buffer g-buf
              (save-excursion (goto-char (point-max))
                              (search-backward "__CAT_DONE__" (max (point-min) (- (point-max) 4000)) t))))
       (setq g-cat-done (float-time)))
      ((> (- (float-time) g-cat-start) 20) nil)
      (t (g-watch-done))))))

(defun g-next (&optional delay)
  (if-let* ((step (pop g-steps)))
      (run-at-time (or delay 0.8) nil
                   (lambda () (condition-case e (funcall step)
                                (error (g-log "エラー: %S" e) (g-finish)))))
    (g-finish)))

(defun g-finish ()
  (g-log "終わり")
  (kill-emacs 0))

;; ghostel が無ければ何もしない
(cond
 ((not (file-directory-p g-lisp))
  (g-log "ghostel の lisp が無いので測らない: %s" g-lisp)
  (kill-emacs 0))
 ((not (file-exists-p (expand-file-name (concat "ghostel-module" module-file-suffix) g-module)))
  (g-log "ghostel の module が無いので測らない: %s" g-module)
  (kill-emacs 0))
 (t
  (add-to-list 'load-path g-lisp)
  (setq ghostel-module-directory (file-name-as-directory g-module))
  (require 'ghostel)
  ;; EmbTerm 側と条件をそろえる（シェルのエイリアスで `cat' が別物になるのを避ける）
  (setq ghostel-shell '("/bin/sh"))
  (setq ghostel-shell-integration nil)
  (g-log "ghostel を読み込んだ（%s）" g-lisp)
  (setq g-steps
        (list
         (lambda ()
           (unless (file-exists-p g-data)
             (call-process "sh" nil nil nil "-c"
                           (format "awk 'BEGIN{for(i=0;i<200000;i++) printf \"%%06d 0123456789abcdefghijklmnopqrstuvwxyz0123456789\\n\", i}' > %s"
                                   (shell-quote-argument g-data))))
           (g-log "データ: %s バイト" (file-attribute-size (file-attributes g-data)))
           (setq g-buf (ghostel))
           (redisplay t)
           (g-log "端末のバッファ: %S" g-buf)
           (g-next 2.0))
         (lambda ()
           ;; 1. 何もしていないとき
           (g-tick-start)
           (g-next 3.0))
         (lambda ()
           (g-tick-stop "何もしていないとき")
           (g-next))
         (lambda ()
           ;; 2. 10 MB の cat（画面に出す）。終わりの印が出るまでの時間も測る
           (setq g-cat-start (float-time) g-cat-done nil)
           (with-current-buffer g-buf
             (ghostel-send-string (format "/bin/cat %s; echo \"__CAT\"\"_DONE__\"\r" (shell-quote-argument g-data))))
           (g-tick-start)
           (g-watch-done)
           (g-next 20.0))
         (lambda ()
           (g-tick-stop "10 MB の cat の最中")
           (g-log "cat が終わるまで: %s"
                  (if g-cat-done (format "%.2f 秒" (- g-cat-done g-cat-start)) "（20 秒では終わらなかった）"))
           (with-current-buffer g-buf
             (g-log "画面の最後の行: %S"
                    (save-excursion (goto-char (point-max)) (buffer-substring-no-properties
                                                             (line-beginning-position -1) (point-max)))))
           (g-next))
         (lambda ()
           (let ((kill-buffer-query-functions nil))
             (when (buffer-live-p g-buf) (kill-buffer g-buf)))
           (g-next 1.0))))
  (defvar g-cat-start nil)
  (defvar g-cat-done nil)
  (run-at-time 1.0 nil #'g-next)
  (run-at-time 120 nil (lambda () (g-log "時間切れ") (g-finish)))))
