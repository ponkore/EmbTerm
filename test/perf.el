;;; perf.el --- 性能の測定（T-054 / T-068 / T-096 / T-067） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   python -I test/perf-cat.py gen target/t054/data      # 初回だけ（10 MB と 100 万行のファイル）
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/perf.el
;;
;; 環境変数 PERF_TAG（既定 "run"）を結果の名前に付ける（変更の前後を並べるため）。
;; 結果は target/t054/: perf-TAG.log（測った値）、cat-TAG.txt（`cat' の所要時間）、module-TAG.log
;; 測るもの:
;;   - 何もしていないときと、`cat'（10 MB・100 万行）の最中の Emacs の 10 ms タイマーの間隔の分布
;;   - module の UI スレッドの 1 フレーム（ロック・写し・描画・Present）の分布（`embterm-module-perf'）
;;   - 入力から表示までの遅れ（cmd のプロンプトへ 1 文字ずつ）
;;   - 画面いっぱいの色付きの文字（描画の重い場合）、上下に分けた 2 ペイン
;;   - 4000x4000 の PNG（Kitty graphics）のデコード
;;   - 端末を 15 個（見えている 4 個 + 隠れている 11 個）開いたときの Emacs のメモリ・CPU・GPU デバイスの数
;; 最後に Emacs を終える（時間の上限でも終える）。`sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst perf-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst perf-out (expand-file-name "target/t054/" perf-root))
(defconst perf-tag (or (getenv "PERF_TAG") "run"))
(defconst perf-log-file (expand-file-name (format "perf-%s.log" perf-tag) perf-out))
(defconst perf-cat-file (expand-file-name (format "cat-%s.txt" perf-tag) perf-out))
(defconst perf-stats-script (expand-file-name "test/proc-stats.ps1" perf-root))
(defconst perf-timeout 600)

(make-directory perf-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (list perf-log-file perf-cat-file))
  (when (file-exists-p f) (delete-file f)))

(defun perf-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil perf-log-file t 'silent)))

(defun perf-win (path)
  "PATH を cmd に渡せる `\\' 区切りにする。"
  (subst-char-in-string ?/ ?\\ (expand-file-name path)))

(defun perf-type (string &rest keys)
  (setq unread-command-events (append unread-command-events (string-to-list string) keys)))

;;;; Emacs のタイマーの間隔

(defvar perf-tick-last nil)
(defvar perf-ticks nil "間隔（ms）の列。")
(defvar perf-tick-timer nil)

(defun perf-tick ()
  (let ((now (float-time)))
    (when perf-tick-last
      (push (* 1000 (- now perf-tick-last)) perf-ticks))
    (setq perf-tick-last now)))

(defun perf-tick-start ()
  (setq perf-tick-last nil perf-ticks nil
        perf-tick-timer (run-at-time 0 0.01 #'perf-tick)))

(defun perf-quantile (sorted q)
  (nth (round (* q (1- (length sorted)))) sorted))

(defun perf-tick-stop (label)
  (cancel-timer perf-tick-timer)
  (let ((v (sort (copy-sequence perf-ticks) #'<)))
    (if (null v)
        (perf-log "タイマー（%s）: 無し" label)
      (perf-log "タイマー（%s）: n=%d p50=%.1f p90=%.1f p99=%.1f max=%.1f ms、30 ms を超えた回数 %d"
                label (length v) (perf-quantile v 0.5) (perf-quantile v 0.9) (perf-quantile v 0.99)
                (car (last v)) (seq-count (lambda (x) (> x 30)) v)))))

;;;; UI スレッドの応答: 50 ms ごとに何もしない頼みごと（存在しないペインを隠す）を積み、取り出されるまでの遅れ（ui.request_delay）を測る

(defvar perf-ping-timer nil)

(defun perf-ping-start (buf)
  (setq perf-ping-timer
        (run-at-time 0 0.05 (lambda ()
                              (when (buffer-live-p buf)
                                (with-current-buffer buf
                                  (embterm-module-hide embterm--term 999)))))))

(defun perf-ping-stop ()
  (when perf-ping-timer
    (cancel-timer perf-ping-timer)
    (setq perf-ping-timer nil)))

;;;; module の測定

(defun perf-report (buf label &optional reset)
  (with-current-buffer buf
    (let ((r (embterm-module-perf embterm--term reset)))
      (perf-log "module（%s）:\n%s" label
                (mapconcat (lambda (l) (concat "    " l)) (split-string (or r "（応答なし）") "\n" t) "\n"))
      r)))

(defun perf-reset (buf)
  (with-current-buffer buf (embterm-module-perf embterm--term t)))

(defun perf-proc (label)
  (let ((out (with-output-to-string
               (call-process "powershell" nil standard-output nil "-NoProfile" "-ExecutionPolicy" "Bypass"
                             "-File" perf-stats-script (number-to-string (emacs-pid))))))
    (perf-log "プロセス（%s）: %s gpu_devices=%s" label (string-trim out)
              (if (fboundp 'embterm-module-gpu-devices) (embterm-module-gpu-devices) "-"))))

(defun perf-cat-done-p (label)
  (and (file-exists-p perf-cat-file)
       (with-temp-buffer
         (insert-file-contents perf-cat-file)
         (re-search-forward (concat "^" (regexp-quote label) " ") nil t))))

(defun perf-cat-command (file label)
  (format "python -I %s cat %s %s %s\r"
          (perf-win "test/perf-cat.py") (perf-win (concat "target/t054/data/" file))
          (perf-win perf-cat-file) label))

;;;; 手順の進め方: (DELAY NAME . FN) は DELAY 秒後に FN、(:wait NAME PRED TIMEOUT) は PRED が真になるまで 0.2 秒ごとに待つ

(defun perf-run (steps)
  (when steps
    (let ((step (car steps)))
      (if (eq (car step) :wait)
          (pcase-let ((`(:wait ,name ,pred ,timeout) step))
            (let ((t0 (float-time)))
              (cl-labels ((poll ()
                            (cond ((condition-case nil (funcall pred) (error nil))
                                   (perf-log "--- 待った: %s（%.1f 秒）" name (- (float-time) t0))
                                   (perf-run (cdr steps)))
                                  ((> (- (float-time) t0) timeout)
                                   (perf-log "--- 時間切れ: %s" name)
                                   (perf-run (cdr steps)))
                                  (t (run-at-time 0.2 nil #'poll)))))
                (poll))))
        (pcase-let ((`(,delay ,name . ,fn) step))
          (run-at-time delay nil
                       (lambda ()
                         (perf-log "--- %s" name)
                         (condition-case err
                             (funcall fn)
                           (error (perf-log "エラー: %S" err)))
                         (perf-run (cdr steps)))))))))

(defvar perf-buf nil "測る端末（cmd）。")
(defvar perf-many nil "端末を何個も開くときのバッファ。")

(defun perf-cat-steps (file label)
  "FILE を `cat' して、その間のタイマーと module の測定を取る手順。"
  `((0.5 ,(format "cat %s（%s）" file label)
         . ,(lambda ()
              (perf-reset perf-buf)
              (perf-tick-start)
              (perf-ping-start perf-buf)
              (with-current-buffer perf-buf
                (embterm-send-string (perf-cat-command file label)))))
    (:wait ,(format "cat %s の終わり" file) ,(lambda () (perf-cat-done-p label)) 120)
    (0.2 ,(format "cat %s の結果" file)
         . ,(lambda ()
              (perf-tick-stop (format "cat %s" label))
              (perf-ping-stop)
              (perf-report perf-buf label)
              (with-current-buffer perf-buf (embterm-send-string "cls\r"))))))

(defconst perf-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 120 40)
              (set-frame-position (selected-frame) 40 40)
              (load (expand-file-name "target/dist/embterm.el" perf-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name (format "module-%s.log" perf-tag) perf-out))
              (perf-proc "端末なし")
              (let ((embterm-shell '("cmd.exe")))
                (setq perf-buf (embterm-new)))))
    (3.0 "何もしていないとき"
         . ,(lambda ()
              (with-current-buffer perf-buf
                (perf-log "端末 %sx%s" (plist-get embterm--state :cols) (plist-get embterm--state :rows)))
              (perf-proc "端末 1 個")
              (perf-tick-start)))
    (3.0 "入力の遅れ: 1 文字ずつ 60 回（0.15 秒おき）"
         . ,(lambda ()
              (perf-tick-stop "何もしていない")
              (perf-type "rem ")
              (perf-reset perf-buf)
              (dotimes (i 60)
                (run-at-time (* 0.15 (1+ i)) nil (lambda () (perf-type "x"))))))
    (10.0 "入力の遅れの結果"
          . ,(lambda ()
               (perf-report perf-buf "入力")
               (perf-type "" 'return)))
    ,@(perf-cat-steps "big10m.txt" "emacs-big10m")
    ,@(perf-cat-steps "lines1m.txt" "emacs-lines1m")
    (1.0 "画面いっぱいの色付きの文字（フレームを最大化）"
         . ,(lambda ()
              (set-frame-parameter nil 'fullscreen 'maximized)))
    (2.0 "入力の遅れ（最大化した 1 ペイン）"
         . ,(lambda ()
              (perf-type "rem ")
              (perf-reset perf-buf)
              (dotimes (i 30)
                (run-at-time (* 0.15 (1+ i)) nil (lambda () (perf-type "x"))))))
    (5.5 "入力の遅れ（最大化）の結果"
         . ,(lambda ()
              (perf-report perf-buf "入力（最大化）")
              (perf-type "" 'return)))
    (2.0 "colors を流す"
         . ,(lambda ()
              (with-current-buffer perf-buf
                (perf-log "端末 %sx%s" (plist-get embterm--state :cols) (plist-get embterm--state :rows))
                (perf-reset perf-buf)
                (perf-tick-start)
                (perf-ping-start perf-buf)
                (embterm-send-string (format "python -I %s colors %s emacs-colors 100\r"
                                             (perf-win "test/perf-cat.py") (perf-win perf-cat-file))))))
    (:wait "colors の終わり" ,(lambda () (perf-cat-done-p "emacs-colors")) 120)
    (0.5 "colors の結果"
         . ,(lambda ()
              (perf-tick-stop "colors（最大化）")
              (perf-ping-stop)
              (perf-report perf-buf "colors（最大化）")
              (set-frame-parameter nil 'fullscreen nil)
              (with-current-buffer perf-buf (embterm-send-string "cls\r"))))
    (1.5 "上下に 2 ペイン"
         . ,(lambda ()
              (split-window-below)))
    ,@(perf-cat-steps "big10m.txt" "emacs-big10m-2panes")
    (0.5 "1 ペインに戻す"
         . ,(lambda () (delete-other-windows)))
    (1.0 "4000x4000 の PNG（Kitty graphics）"
         . ,(lambda ()
              (perf-reset perf-buf)
              (perf-tick-start)
              (perf-ping-start perf-buf)
              (with-current-buffer perf-buf
                (embterm-send-string (format "python -I %s big --no-wait\r" (perf-win "test/kitty-graphics.py"))))))
    (:wait "PNG のデコード"
           ,(lambda () (with-current-buffer perf-buf
                         (string-match-p "png\\.decode n=[1-9]" (or (embterm-module-perf embterm--term nil) ""))))
           60)
    (2.0 "PNG の結果"
         . ,(lambda ()
              (perf-tick-stop "PNG")
              (perf-ping-stop)
              (perf-report perf-buf "PNG")
              (with-current-buffer perf-buf (embterm-send-string "cls\r"))))
    (1.0 "端末を 14 個足す（合わせて 15 個）"
         . ,(lambda ()
              (perf-proc "端末 1 個（PNG のあと）")
              (let ((embterm-shell '("cmd.exe")))
                (dotimes (_ 14)
                  (push (embterm-new) perf-many)))
              (setq perf-many (nreverse perf-many))))
    (2.0 "2x2 に 4 個を出す（残り 11 個は隠れる）"
         . ,(lambda ()
              (delete-other-windows)
              (switch-to-buffer perf-buf)
              (let* ((w1 (selected-window))
                     (w2 (split-window-right))
                     (w3 (split-window-below))
                     (w4 (with-selected-window w2 (split-window-below))))
                (set-window-buffer w2 (nth 0 perf-many))
                (set-window-buffer w3 (nth 1 perf-many))
                (set-window-buffer w4 (nth 2 perf-many))
                (select-window w1))))
    (6.0 "15 個（4 個が見える）で何もしていないとき"
         . ,(lambda ()
              (perf-proc "端末 15 個（idle の始め）")
              (perf-tick-start)))
    (10.0 "10 秒後"
          . ,(lambda ()
               (perf-tick-stop "15 個で何もしていない")
               (perf-proc "端末 15 個（idle の 10 秒後。タイマーあり）")))
    (10.0 "さらに 10 秒（測定のタイマーなし）"
          . ,(lambda ()
               (perf-proc "端末 15 個（さらに 10 秒後。タイマーなし）")))
    ,@(perf-cat-steps "big10m.txt" "emacs-big10m-15terms")
    (0.5 "15 個で cat した後"
         . ,(lambda () (perf-proc "端末 15 個（cat の後）")))
    (1.0 "全部消す"
         . ,(lambda ()
              (let ((t0 (float-time)))
                (dolist (b (cons perf-buf perf-many))
                  (when (buffer-live-p b) (kill-buffer b)))
                (perf-log "kill-buffer 15 個: %.1f ms" (* 1000 (- (float-time) t0))))))
    (6.0 "消した 6 秒後"
         . ,(lambda () (perf-proc "全部消した後")))
    (1.0 "終わり"
         . ,(lambda ()
              (perf-log "終える")
              (kill-emacs 0)))))

(run-at-time perf-timeout nil (lambda () (perf-log "時間切れで終える") (kill-emacs 2)))
(perf-log "perf.el を始める（Emacs %s、タグ %s）" emacs-version perf-tag)
(perf-run perf-steps)

;;; perf.el ends here
