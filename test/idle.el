;;; idle.el --- 何もしていないときの CPU とクリックへの反応（T-104）の測定と確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/idle.el                 # module からの知らせを使う（既定）
;;   IDLE_POLL=1 C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/idle.el     # 使わない（50 ms ごとのポーリング。比べる用）
;;
;; 端末を 15 個（4 個を 2x2 に見せ、11 個は隠す）開いて 10 秒何もせず、Emacs の CPU 時間を測る。続けてペインをクリックして
;; そのウィンドウが選ばれるまでの時間と、出力からタイトルがバッファ名に入るまでの時間を測る。
;; 結果は target/t104/idle-<poll|notify>.log。知らせを使うときは CPU とクリックの遅れに上限を置いて OK / NG を出す。
;; `sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst idle-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst idle-out (expand-file-name "target/t104/" idle-root))
(defconst idle-poll (getenv "IDLE_POLL"))
(defconst idle-log-file (expand-file-name (if idle-poll "idle-poll.log" "idle-notify.log") idle-out))
(defconst idle-mouse (expand-file-name "test/post-mouse.ps1" idle-root))

(make-directory idle-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p idle-log-file) (delete-file idle-log-file))

(defvar idle-ng 0)
(defvar idle-bufs nil)
(defvar idle-cpu0 nil)
(defvar idle-t0 nil)
(defvar idle-polls 0 "`embterm--poll' の回数（何もしていない間に知らせが来ていないかを見る）。")
(advice-add 'embterm--poll :before (lambda (&rest _) (cl-incf idle-polls)))

(defun idle-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args)) nil idle-log-file t 'silent))

(defun idle-check (name ok &optional detail)
  (unless ok (cl-incf idle-ng))
  (idle-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun idle-cpu ()
  "この Emacs の CPU 時間（秒）。"
  (let ((a (process-attributes (emacs-pid))))
    (+ (float-time (alist-get 'utime a)) (float-time (alist-get 'stime a)))))

(defun idle-wait (pred timeout then)
  "PRED が真になるまで 5 ms ごとに見て、経った秒数（時間切れなら nil）で THEN を呼ぶ。"
  (let ((t0 (float-time)) timer)
    (setq timer (run-at-time 0 0.005
                             (lambda ()
                               (let ((dt (- (float-time) t0)))
                                 (cond ((funcall pred) (cancel-timer timer) (funcall then dt))
                                       ((> dt timeout) (cancel-timer timer) (funcall then nil)))))))))

(add-to-list 'load-path (expand-file-name "target/dist" idle-root))
(require 'embterm)
(when idle-poll (setq embterm-use-notify nil))
(set-frame-size nil 140 44)

(defun idle-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (idle-check "エラーなし" nil (format "%S" err))))
                   (idle-run (cdr steps))))))

(idle-run
 `((0.5 . ,(lambda ()
             (let ((embterm-shell '("cmd.exe" "/d" "/q" "/k" "prompt $g")))
               (dotimes (_ 15) (push (embterm-new) idle-bufs)))
             (setq idle-bufs (nreverse idle-bufs))
             ;; 2x2 に 4 個を見せる
             (delete-other-windows)
             (switch-to-buffer (nth 0 idle-bufs))
             (let* ((w1 (selected-window)) (w2 (split-window-right)) (w3 (split-window-below)) (w4 (with-selected-window w2 (split-window-below))))
               (set-window-buffer w2 (nth 1 idle-bufs))
               (set-window-buffer w3 (nth 2 idle-bufs))
               (set-window-buffer w4 (nth 3 idle-bufs))
               (select-window w1))))
   (4.0 . ,(lambda ()
             (idle-log "知らせ: %s（embterm-use-notify %s）" embterm--notify-connected embterm-use-notify)
             (unless idle-poll
               (idle-check "module からの知らせがつながった" embterm--notify-connected))
             (setq idle-cpu0 (idle-cpu) idle-t0 (float-time) idle-polls 0)))
   (10.0 . ,(lambda ()
              (let* ((cpu (- (idle-cpu) idle-cpu0)) (wall (- (float-time) idle-t0)) (pct (* 100 (/ cpu wall))))
                (idle-log "端末 15 個で何もしていない %.1f 秒: Emacs の CPU %.3f 秒（%.2f%%）、見回り %d 回" wall cpu pct idle-polls)
                ;; CPU 時間は 15.6 ms 刻みで、Emacs 自身の処理も入るので揺れる（0.3〜1.6%）。見回りの回数で、余計な知らせが
                ;; 来ていないことを確かめる（0.5 秒ごとの念のための見回りだけなら 10 秒で 20 回）
                (unless idle-poll
                  (idle-check "何もしていない間は念のための見回りだけ（10 秒で 25 回以下）" (<= idle-polls 25) (format "%d 回" idle-polls))
                  (idle-check "何もしていないときの CPU が 3% 未満（ポーリングだけのときは 6%）" (< pct 3.0) (format "%.2f%%" pct))))
              ;; 右下のペインをクリックして、そのウィンドウが選ばれるまで
              (let* ((buf (nth 3 idle-bufs))
                     (win (get-buffer-window buf))
                     (hwnd (plist-get (buffer-local-value 'embterm--state buf) :hwnd)))
                (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File" idle-mouse
                              (number-to-string hwnd) "down:20:20,up:20:20")
                (idle-wait (lambda () (eq (selected-window) win)) 2
                           (lambda (dt)
                             (idle-log "クリックからウィンドウが選ばれるまで: %s" (if dt (format "%.0f ms" (* 1000 dt)) "時間切れ"))
                             (unless idle-poll
                               (idle-check "クリックから 100 ms 以内に選ばれる" (and dt (< dt 0.1)))))))))
   (1.5 . ,(lambda ()
             ;; 出力（タイトル）からバッファ名が変わるまで
             (let ((buf (nth 4 idle-bufs)))
               (with-current-buffer buf (embterm-send-string "title idle-title-x\r"))
               (idle-wait (lambda () (string-match-p "idle-title-x" (buffer-name buf))) 2
                          (lambda (dt)
                            (idle-log "タイトルがバッファ名に入るまで（隠れた端末）: %s" (if dt (format "%.0f ms" (* 1000 dt)) "時間切れ"))
                            (idle-check "隠れた端末のタイトルも届く" dt))))))
   (1.5 . ,(lambda ()
             (idle-log "失敗の数: %d" idle-ng)
             (kill-emacs (if (zerop idle-ng) 0 1))))))

(run-at-time 60 nil (lambda () (idle-log "時間切れ") (kill-emacs 2)))

;;; idle.el ends here
