;;; smoke.el --- EmbTerm の module と embterm.el の自動確認（T-028 / T-029） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/runemacs.exe -Q -l test/smoke.el
;;
;; target/dist の embterm.el と module を読み、cmd と pwsh の端末を開いて、キーを送り、分割・リサイズ・
;; バッファの切り替え・フレームの削除・copy モードを順に試す。結果は target/t028/ に書く:
;;   smoke.log         手順ごとの状態（module の状態の plist など）
;;   *.png             端末の画面（module の保存）と、Emacs のフレーム全体（frame-*.png。PrintWindow）
;;   *.txt             端末の画面のテキスト
;; 最後に Emacs を終える（時間の上限 `smoke-timeout' 秒でも終える）。
;; 注意: `sit-for' は使わない（待ちはすべて `run-at-time' の間隔で作る）

(require 'cl-lib)
(require 'seq)

(defconst smoke-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst smoke-out (expand-file-name "target/t028/" smoke-root))
(defconst smoke-log-file (expand-file-name "smoke.log" smoke-out))
(defconst smoke-capture-script (expand-file-name "test/capture-window.ps1" smoke-root))
(defconst smoke-timeout 150 "この秒数が経ったら Emacs を終える。")

(make-directory smoke-out t)
;; 書き出しは UTF-8（既定の cp932 で書けない文字があると、書く前に尋ねてきて止まる）
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p smoke-log-file)
  (delete-file smoke-log-file))

(defun smoke-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil smoke-log-file t 'silent)))

(defun smoke-out (name)
  (expand-file-name name smoke-out))

(defun smoke-capture-frame (name &optional frame)
  "FRAME（既定は選んでいるフレーム）を PrintWindow で NAME に保存する。"
  (redisplay t)
  (let* ((id (frame-parameter (or frame (selected-frame)) 'window-id))
         (out (smoke-out name))
         (status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass"
                               "-File" smoke-capture-script id out)))
    (smoke-log "フレームを保存: %s（%s）" out status)))

(defun smoke-state (buf)
  (if (buffer-live-p buf)
      (with-current-buffer buf
        (format "win=%s mode=%s state=%S" embterm--window embterm--input-mode
                (cl-loop for (k v) on embterm--state by #'cddr
                         unless (eq k :events) append (list k v))))
    "（バッファは消えた）"))

(defun smoke-type (string &rest keys)
  "STRING の文字と、続く KEYS（`return' などのイベント）を、本物のキー入力として積む。"
  (setq unread-command-events (append unread-command-events (string-to-list string) keys)))

(defun smoke-save-text (buf name)
  (with-current-buffer buf
    (let ((text (embterm-screen-text)))
      (write-region (or text "") nil (smoke-out name) nil 'silent)
      (smoke-log "画面のテキスト %s:\n%s" name
                 (mapconcat (lambda (l) (concat "  | " l))
                            (seq-take (seq-remove #'string-empty-p (split-string (or text "") "\n")) 20)
                            "\n")))))

(defun smoke-screenshot (buf name)
  (with-current-buffer buf
    (embterm-screenshot (smoke-out name))))

;;;; 応答の確かめ（Emacs が固まらないか）: 10 ms ごとのタイマーの間隔の最大を測る

(defvar smoke-tick-last nil)
(defvar smoke-tick-max 0)
(defvar smoke-tick-count 0)
(defvar smoke-tick-timer nil)

(defun smoke-tick ()
  (let ((now (float-time)))
    (when smoke-tick-last
      (setq smoke-tick-max (max smoke-tick-max (- now smoke-tick-last))))
    (setq smoke-tick-last now
          smoke-tick-count (1+ smoke-tick-count))))

(defun smoke-tick-start ()
  (setq smoke-tick-last nil smoke-tick-max 0 smoke-tick-count 0
        smoke-tick-timer (run-at-time 0 0.01 #'smoke-tick)))

(defun smoke-tick-stop (label)
  (cancel-timer smoke-tick-timer)
  (smoke-log "応答（%s）: タイマー %d 回、間隔の最大 %.0f ms" label smoke-tick-count (* 1000 smoke-tick-max)))

;;;; 手順

(defvar smoke-cmd nil "cmd の端末のバッファ。")
(defvar smoke-pwsh nil "pwsh の端末のバッファ。")
(defvar smoke-pids nil "子プロセスの pid。")
(defvar smoke-frame2 nil)

(defun smoke-pid (buf)
  (and (buffer-live-p buf) (plist-get (buffer-local-value 'embterm--state buf) :pid)))

(defun smoke-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (smoke-log "--- %s" name)
                     (condition-case err
                         (funcall fn)
                       (error (smoke-log "エラー: %S" err)))
                     (smoke-run (cdr steps)))))))

(defconst smoke-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 110 36)
              (set-frame-position (selected-frame) 40 40)
              (load (expand-file-name "target/dist/embterm.el" smoke-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (smoke-out "module.log"))
              (smoke-log "module %s / %s" embterm--module-loaded embterm-module-directory)
              (let ((embterm-shell '("cmd.exe")))
                (setq smoke-cmd (embterm)))
              (smoke-log "%s" (smoke-state smoke-cmd))))
    (3.0 "cmd: キーを送る（semi-char のキーマップを通す）"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-type "echo ①あ★─" 'return)
              (smoke-type "dir" 'return)))
    (3.0 "cmd: 画面を保存"
         . ,(lambda ()
              (smoke-screenshot smoke-cmd "cmd1.png")
              (smoke-save-text smoke-cmd "cmd1.txt")
              (smoke-capture-frame "frame-cmd1.png")
              (smoke-log "%s" (smoke-state smoke-cmd))))
    (1.0 "C-x 2（上下に分割。選んでいる上のウィンドウに出る）"
         . ,(lambda ()
              (split-window-below)
              (smoke-log "%s / 選択 %s" (smoke-state smoke-cmd) (selected-window))))
    (1.5 "分割を保存"
         . ,(lambda ()
              (smoke-capture-frame "frame-split-v-top.png")
              (smoke-log "%s" (smoke-state smoke-cmd))
              (other-window 1)))
    (1.5 "下のウィンドウを選ぶ（ビューが付いていく）"
         . ,(lambda ()
              (smoke-capture-frame "frame-split-v-bottom.png")
              (smoke-log "%s / 選択 %s" (smoke-state smoke-cmd) (selected-window))
              (delete-other-windows)
              (split-window-right)))
    (1.5 "C-x 3（左右に分割。桁数が半分ほどになる）"
         . ,(lambda ()
              (smoke-capture-frame "frame-split-h.png")
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-screenshot smoke-cmd "split-h.png")
              (delete-other-windows)))
    (1.5 "分割を戻す"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (set-frame-size (selected-frame) 80 28)))
    (1.5 "フレームを小さくした（桁数の追従）"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-type "dir /w" 'return)))
    (2.0 "小さくした画面を保存"
         . ,(lambda ()
              (smoke-screenshot smoke-cmd "resize.png")
              (smoke-save-text smoke-cmd "resize.txt")
              (smoke-capture-frame "frame-resize.png")
              (set-frame-size (selected-frame) 110 36)))
    (1.5 "バッファを切り替える（ビューが隠れる）"
         . ,(lambda ()
              (switch-to-buffer "*scratch*")))
    (1.0 "隠れたところを保存"
         . ,(lambda ()
              (smoke-capture-frame "frame-hidden.png")
              (smoke-log "%s" (smoke-state smoke-cmd))
              (switch-to-buffer smoke-cmd)))
    (1.0 "戻した"
         . ,(lambda ()
              (smoke-capture-frame "frame-back.png")
              (smoke-log "%s" (smoke-state smoke-cmd))
              ;; C-c C-k で copy モード
              (smoke-type "" ?\C-c ?\C-k)))
    (1.0 "copy モード"
         . ,(lambda ()
              (smoke-capture-frame "frame-copy.png")
              (smoke-log "%s / バッファの先頭: %S" (smoke-state smoke-cmd)
                         (with-current-buffer smoke-cmd
                           (buffer-substring-no-properties (point-min) (min (point-max) 200))))
              (smoke-type "" ?\C-c ?\C-j)))
    (1.0 "semi-char に戻した"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-type "echo ①あ★─ x※y" 'return)
              ;; 設定の変更（フォントの大きさと広いグリフの描き方）が開いている端末に届くか
              (customize-set-variable 'embterm-font-size 16)
              (customize-set-variable 'embterm-narrow-wide 'shrink)))
    (1.5 "フォント 16pt + shrink"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-screenshot smoke-cmd "font16-shrink.png")))
    (1.0 "設定を戻す"
         . ,(lambda ()
              (customize-set-variable 'embterm-font-size nil)
              (customize-set-variable 'embterm-narrow-wide 'overlap)))
    (1.0 "設定を戻した"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (let ((embterm-shell '("pwsh" "-NoLogo")))
                (setq smoke-pwsh (embterm)))))
    (4.0 "pwsh: キーを送る"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-pwsh))
              (smoke-type "Write-Output \"pwsh ok $($PSVersionTable.PSVersion) ①あ★─\"" 'return)))
    (3.0 "pwsh: 画面を保存"
         . ,(lambda ()
              (smoke-screenshot smoke-pwsh "pwsh.png")
              (smoke-save-text smoke-pwsh "pwsh.txt")
              (smoke-capture-frame "frame-pwsh.png")
              (smoke-tick-start)))
    (2.0 "応答: 何もしていないとき"
         . ,(lambda ()
              (smoke-tick-stop "何もしていない")
              (smoke-type "1..30000 | % { \"line $_ ①あ★─ abcdefghijklmnopqrstuvwxyz\" }" 'return)
              (smoke-tick-start)))
    (6.0 "応答: 大量の出力の最中"
         . ,(lambda ()
              (smoke-tick-stop "大量の出力の最中")
              (smoke-screenshot smoke-pwsh "pwsh-flood.png")
              (smoke-log "%s" (smoke-state smoke-pwsh))))
    (6.0 "左右に cmd と pwsh を並べる（左を選ぶ）"
         . ,(lambda ()
              (delete-other-windows)
              (switch-to-buffer smoke-cmd)
              (split-window-right)
              (other-window 1)
              (switch-to-buffer smoke-pwsh)
              (other-window -1)))
    (1.5 "右の pwsh の子 HWND をクリックする（右のウィンドウが選ばれる）"
         . ,(lambda ()
              (smoke-capture-frame "frame-two.png")
              (smoke-log "選択: %s" (selected-window))
              (let ((hwnd (plist-get (buffer-local-value 'embterm--state smoke-pwsh) :hwnd)))
                (smoke-log "クリック: %s"
                           (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass"
                                         "-File" (expand-file-name "test/click-window.ps1" smoke-root)
                                         (number-to-string hwnd) "30" "30")))))
    (1.0 "クリックの後"
         . ,(lambda ()
              (smoke-log "選択: %s（pwsh なら成功: %s）" (selected-window)
                         (eq (window-buffer (selected-window)) smoke-pwsh))
              (smoke-capture-frame "frame-click.png")
              (delete-other-windows)))
    (1.0 "別のフレームに cmd を出す"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-pwsh))
              (switch-to-buffer "*scratch*")
              (setq smoke-frame2 (make-frame '((width . 90) (height . 25) (left . 300) (top . 200))))
              (select-frame smoke-frame2)
              (switch-to-buffer smoke-cmd)))
    (2.0 "別のフレーム"
         . ,(lambda ()
              (smoke-capture-frame "frame2.png" smoke-frame2)
              (smoke-log "%s" (smoke-state smoke-cmd))
              (delete-frame smoke-frame2)))
    (1.5 "フレームを消した（子 HWND は消え、端末は残る）"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (switch-to-buffer smoke-cmd)))
    (2.0 "元のフレームに戻した（子 HWND を作り直す）"
         . ,(lambda ()
              (smoke-log "%s" (smoke-state smoke-cmd))
              (smoke-type "echo after-frame-delete" 'return)))
    (2.0 "作り直した画面を保存"
         . ,(lambda ()
              (smoke-screenshot smoke-cmd "after-frame-delete.png")
              (smoke-save-text smoke-cmd "after-frame-delete.txt")
              (smoke-capture-frame "frame-after-delete.png")
              (setq smoke-pids (list (smoke-pid smoke-cmd) (smoke-pid smoke-pwsh)))
              (smoke-log "pid: %S / 生きているか: %S" smoke-pids
                         (mapcar (lambda (p) (and p (process-attributes p) t)) smoke-pids))
              (let ((t0 (float-time)))
                (kill-buffer smoke-cmd)
                (kill-buffer smoke-pwsh)
                (smoke-log "kill-buffer 2 つにかかった時間: %.1f ms（閉じる処理を待たないこと）"
                           (* 1000 (- (float-time) t0))))))
    (5.0 "バッファを消した（プロセスが消える）"
         . ,(lambda ()
              (smoke-log "pid: %S / 生きているか: %S / 端末のバッファ: %S" smoke-pids
                         (mapcar (lambda (p) (and p (process-attributes p) t)) smoke-pids)
                         embterm--buffers)
              (let ((embterm-shell '("cmd.exe" "/c" "exit 3")))
                (setq smoke-cmd (embterm)))))
    (3.0 "終了コード 3 で終わる端末（バッファが消える）"
         . ,(lambda ()
              (smoke-log "バッファは生きているか: %S / 端末のバッファ: %S" (buffer-live-p smoke-cmd) embterm--buffers)
              (smoke-log "*Messages* の末尾:\n%s"
                         (with-current-buffer "*Messages*"
                           (buffer-substring-no-properties (max (point-min) (- (point-max) 1500)) (point-max))))))
    (1.0 "終わり"
         . ,(lambda ()
              (smoke-log "終える")
              (kill-emacs 0)))))

(run-at-time smoke-timeout nil (lambda () (smoke-log "時間切れで終える") (kill-emacs 2)))
(smoke-log "smoke.el を始める（Emacs %s）" emacs-version)
(smoke-run smoke-steps)

;;; smoke.el ends here
