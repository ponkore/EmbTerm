;;; kitty.el --- Kitty graphics を別に起こした Emacs の中で確かめる（T-088 / T-052） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/kitty.el
;;
;; yazi の画像プレビューと、画像がペインの位置・大きさに合うかを PNG に撮る。
;; 結果は target/t088/emacs/ に書く: kitty.log（OK / NG）、frame-*.png（フレーム全体）
;; 最後に Emacs を終える。`sit-for' は使わない。yazi の設定は一時ディレクトリを `YAZI_CONFIG_HOME' で指す。

(require 'cl-lib)
(require 'seq)

(defconst kitty-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst kitty-out (expand-file-name "target/t088/emacs/" kitty-root))
(defconst kitty-log-file (expand-file-name "kitty.log" kitty-out))
(defconst kitty-capture (expand-file-name "test/capture-window.ps1" kitty-root))
(defconst kitty-mouse (expand-file-name "test/post-mouse.ps1" kitty-root))
(defconst kitty-imgs (expand-file-name "target/t088/imgs" kitty-root))
(defconst kitty-yazi-config (expand-file-name "target/t088/yazi-config" kitty-root))
(defconst kitty-timeout 120)

(make-directory kitty-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p kitty-log-file)
  (delete-file kitty-log-file))

(defvar kitty-ng 0 "NG の数。")
(defvar kitty-buf nil)

(defun kitty-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil kitty-log-file t 'silent)))

(defun kitty-check (name ok &optional detail)
  (unless ok (cl-incf kitty-ng))
  (kitty-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun kitty-frame-png (name)
  (redisplay t)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              kitty-capture (frame-parameter nil 'window-id) (expand-file-name name kitty-out))))
    (kitty-log "フレームを保存: %s（%s）" name status)))

(defun kitty-type (string &rest keys)
  (setq unread-command-events (append unread-command-events (string-to-list string) keys)))

(defun kitty-post (hwnd actions)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              kitty-mouse (number-to-string hwnd) actions)))
    (kitty-log "マウス: %s（%s）" actions status)))

(defun kitty-panes ()
  "ペインの一覧 ((ID HWND COLS ROWS SCROLLED) ...)。ウィンドウの並びの順。"
  (with-current-buffer kitty-buf
    (embterm--poll-1)
    (let ((infos (plist-get embterm--state :panes)))
      (delq nil (mapcar (lambda (p) (assq (embterm--pane-id p) infos)) embterm--panes)))))

(defun kitty-pane-text (info)
  (with-current-buffer kitty-buf
    (embterm-module-pane-text embterm--term (car info))))

(defun kitty-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (kitty-log "--- %s" name)
                     (condition-case err
                         (funcall fn)
                       (error (cl-incf kitty-ng) (kitty-log "NG エラー: %S" err)))
                     (kitty-run (cdr steps)))))))

(defconst kitty-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 130 45)
              (set-frame-position (selected-frame) 30 30)
              (load (expand-file-name "target/dist/embterm.el" kitty-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module.log" kitty-out))
              (let ((embterm-shell '("cmd.exe")))
                (setq kitty-buf (embterm)))))
    (3.0 "yazi を起動して画像を選ぶ"
         . ,(lambda ()
              (kitty-type (format "set YAZI_CONFIG_HOME=%s&& yazi %s" kitty-yazi-config kitty-imgs) 'return)))
    (7.0 "yazi の画面（フレームの PNG）"
         . ,(lambda ()
              (let ((p (car (kitty-panes))))
                (kitty-check "yazi が画像ファイルを見ている" (string-match-p "a-gradient" (or (kitty-pane-text p) ""))))
              (kitty-frame-png "frame-yazi.png")
              (kitty-type "j")))
    (3.0 "次の画像へ（j）"
         . ,(lambda ()
              (kitty-frame-png "frame-yazi-j.png")
              (split-window-right)))
    (2.0 "左右に分割しても崩れない（yazi のまま）"
         . ,(lambda ()
              (kitty-check "ペインが 2 つ" (= (length (kitty-panes)) 2))
              (kitty-frame-png "frame-yazi-split-right.png")
              (kitty-type "q")))
    (2.5 "yazi を終える。1 ウィンドウに戻して、広い端末で画像を出す（基本のシーン）"
         . ,(lambda ()
              (delete-other-windows)
              (kitty-type "cls" 'return)))
    (1.0 "基本のシーン"
         . ,(lambda ()
              (kitty-type (format "python -I %s/test/kitty-graphics.py basic --no-wait" kitty-root) 'return)))
    (2.5 "幅の狭いペイン（右）で切り取られる"
         . ,(lambda ()
              (split-window-right 96)
              (other-window 1)))
    (2.0 "右のペインを選ぶと端末が小さくなる。左へ戻して、右は切り取りのまま"
         . ,(lambda ()
              (other-window 1)))
    (2.0 "狭いペインの写真"
         . ,(lambda ()
              (kitty-check "ペインが 2 つ" (= (length (kitty-panes)) 2))
              (kitty-frame-png "frame-basic-narrow.png")
              (delete-other-windows)
              (kitty-type "cls" 'return)))
    (1.0 "スクロールで流れた画像（上が選んでいる小さいペイン、下が大きいペイン）"
         . ,(lambda ()
              (split-window-below)
              (enlarge-window (- 12 (window-height)))
              (kitty-type (format "python -I %s/test/kitty-graphics.py scroll --no-wait 12" kitty-root) 'return)))
    (3.0 "下のペインは端末より行が多い（overscan）。画像はそのペインでも正しい位置に出る"
         . ,(lambda ()
              (let ((p (kitty-panes)))
                (kitty-check "ペインが 2 つ" (= (length p) 2) (format "%S" p))
                (kitty-log "ペイン: %S" p)
                ;; 上が選んでいるペイン（小さい）、下が大きいペイン（端末より行が多く overscan で埋める）
                (kitty-check "大きいペインは最初の行まで見える"
                             (string-match-p "scroll: image at line 2" (or (kitty-pane-text (nth 1 p)) ""))))
              (kitty-frame-png "frame-overscan.png")
              (kitty-type (format "python -I %s/test/kitty-graphics.py scroll --no-wait 40" kitty-root) 'return)))
    (3.0 "長い出力のあと、上のペインだけ遡る"
         . ,(lambda ()
              (let ((p (kitty-panes)))
                ;; 上の小さいペイン（9 行）を 39 行遡る（1 ノッチ 3 行）。画像の途中がペインの上端で切れる位置
                (kitty-post (nth 1 (car p)) "wheel:360:50:50,wheel:360:50:50,wheel:360:50:50,wheel:360:50:50,wheel:120:50:50"))))
    (1.5 "遡ったペインに画像が出る"
         . ,(lambda ()
              (let ((p (kitty-panes)))
                (kitty-check "上は遡っている" (nth 4 (car p)) (format "%S" p))
                (kitty-check "遡ったペインに画像の直後の行が見える"
                             (string-match-p "line 0" (or (kitty-pane-text (car p)) ""))
                             (format "%S" (seq-take (split-string (or (kitty-pane-text (car p)) "") "\n") 4))))
              (kitty-frame-png "frame-scrolled.png")))
    (1.0 "終わり"
         . ,(lambda ()
              (kitty-log "NG の数: %d" kitty-ng)
              (kill-emacs (if (zerop kitty-ng) 0 3))))))

(run-at-time kitty-timeout nil (lambda () (kitty-log "時間切れで終える") (kill-emacs 2)))
(kitty-log "kitty.el を始める（Emacs %s）" emacs-version)
(kitty-run kitty-steps)

;;; kitty.el ends here
