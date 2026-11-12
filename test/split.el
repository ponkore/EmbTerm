;;; split.el --- split・スクロールバック・選択・copy モードの自動確認（T-063 / T-037 / T-036 / T-017） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/split.el
;;
;; 結果は target/t063/ に書く: split.log（確かめた結果の OK / NG）、frame-*.png（フレーム全体）、*.txt
;; 最後に Emacs を終える（時間の上限でも終える）。`sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst split-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst split-out (expand-file-name "target/t063/" split-root))
(defconst split-log-file (expand-file-name "split.log" split-out))
(defconst split-capture (expand-file-name "test/capture-window.ps1" split-root))
(defconst split-mouse (expand-file-name "test/post-mouse.ps1" split-root))
(defconst split-timeout 180)

(make-directory split-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p split-log-file)
  (delete-file split-log-file))

(defvar split-ng 0 "NG の数。")

(defun split-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil split-log-file t 'silent)))

(defun split-check (name ok &optional detail)
  (unless ok (cl-incf split-ng))
  (split-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun split-frame-png (name)
  (redisplay t)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              split-capture (frame-parameter nil 'window-id) (expand-file-name name split-out))))
    (split-log "フレームを保存: %s（%s）" name status)))

(defun split-type (string &rest keys)
  (setq unread-command-events (append unread-command-events (string-to-list string) keys)))

(defun split-post (hwnd actions)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              split-mouse (number-to-string hwnd) actions)))
    (split-log "マウス: %s（%s）" actions status)))

(defvar split-clipboard nil "`kill-new' が interprogram-cut-function に渡した文字列（クリップボードへの書き込みの代わりに見る）。")
(advice-add 'gui-select-text :before (lambda (text) (setq split-clipboard text)))

(defvar split-buf nil)
(defvar split-buf2 nil)

(defun split-panes ()
  "ペインの一覧 ((ID HWND COLS ROWS SCROLLED) ...)。ウィンドウの並びの順。"
  (with-current-buffer split-buf
    (embterm--poll-1)
    (let ((infos (plist-get embterm--state :panes)))
      (delq nil (mapcar (lambda (p)
                          (assq (embterm--pane-id p) infos))
                        embterm--panes)))))

(defun split-pane-text (info)
  (with-current-buffer split-buf
    (embterm-module-pane-text embterm--term (car info))))

(defun split-pane-hwnd (info) (nth 1 info))
(defun split-pane-size (info) (cons (nth 2 info) (nth 3 info)))
(defun split-pane-scrolled (info) (nth 4 info))

(defun split-state ()
  (with-current-buffer split-buf
    (embterm--poll-1)
    (format "panes=%S cols=%s rows=%s" (plist-get embterm--state :panes)
            (plist-get embterm--state :cols) (plist-get embterm--state :rows))))

(defun split-lines (text) (seq-remove #'string-empty-p (split-string (or text "") "\n")))

(defvar split-top-text nil "遡ったペインのテキスト。")

(defun split-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (split-log "--- %s" name)
                     (condition-case err
                         (funcall fn)
                       (error (cl-incf split-ng) (split-log "NG エラー: %S" err)))
                     (split-run (cdr steps)))))))

(defconst split-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 120 40)
              (set-frame-position (selected-frame) 30 30)
              (load (expand-file-name "target/dist/embterm.el" split-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module.log" split-out))
              (setq embterm-scrollback-lines 2000)
              (let ((embterm-shell '("cmd.exe")))
                (setq split-buf (embterm-new)))))
    (3.0 "出力を出す（スクロールバックを作る）"
         . ,(lambda ()
              (split-log "%s" (split-state))
              (split-type "for /l %i in (1,1,400) do @echo line%i" 'return)))
    (5.0 "C-x 2（上下に分割）"
         . ,(lambda ()
              (split-log "%s" (split-state))
              (split-window-below)))
    (1.5 "両方のウィンドウに端末が見える"
         . ,(lambda ()
              (let ((p (split-panes)))
                (split-check "ペインが 2 つ" (= (length p) 2) (format "%S" p))
                (split-check "両方に HWND" (seq-every-p (lambda (i) (/= 0 (split-pane-hwnd i))) p))
                (split-check "下のペインにも最後の行が見える"
                             (string-match-p "line400" (or (split-pane-text (nth 1 p)) "")) ))
              (split-frame-png "frame-split-v.png")))
    (0.5 "上のペインだけホイールで遡る"
         . ,(lambda ()
              (let ((p (split-panes)))
                (split-post (split-pane-hwnd (nth 0 p)) "wheel:360:50:50,wheel:360:50:50,wheel:360:50:50"))))
    (1.0 "遡った確認（上のペインだけ遡り、内容を覚える）→ 出力を足す"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)) (bottom (nth 1 p)))
                (split-check "上のペインは遡っている" (split-pane-scrolled top) (format "%S" top))
                (split-check "下のペインは追っている" (not (split-pane-scrolled bottom)))
                (setq split-top-text (split-pane-text top))
                (split-log "上のペイン: %S" (split-lines split-top-text))
                (split-frame-png "frame-scrolled.png"))
              (select-window (next-window))
              (split-type "for /l %i in (1,1,30) do @echo more%i" 'return)))
    (3.0 "出力が増えても遡ったほうは動かない"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)) (bottom (nth 1 p))
                     (bt (split-pane-text bottom)) (tt (split-pane-text top)))
                (split-check "遡ったペインの内容が同じ" (equal tt split-top-text)
                             (format "%S" (split-lines tt)))
                (split-check "下のペインに新しい行" (string-match-p "more30" (or bt "")) (format "%S" (seq-take (split-lines bt) 5))))
              (split-frame-png "frame-output-while-scrolled.png")))
    (0.5 "入力したら選んでいるペイン（下）はいちばん下（上は遡ったまま）"
         . ,(lambda ()
              (split-type "echo typed" 'return)))
    (2.0 "入力の結果"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-check "上は遡ったまま" (split-pane-scrolled top)))))
    (0.2 "S-<prior> / S-<end> で選んでいるペインをスクロール"
         . ,(lambda ()
              (split-log "%s" (split-state))
              (split-type "" 'S-prior)))
    (1.0 "S-<prior> の後"
         . ,(lambda ()
              (let* ((p (split-panes)) (bottom (nth 1 p)))
                (split-check "下のペインが遡った" (split-pane-scrolled bottom))
                (split-log "下: %S" (split-lines (split-pane-text bottom))))
              (split-type "" 'S-end)))
    (1.0 "S-<end> の後"
         . ,(lambda ()
              (let* ((p (split-panes)) (bottom (nth 1 p)))
                (split-check "下のペインが下へ戻った" (not (split-pane-scrolled bottom))))))
    (0.2 "ダブルクリック（単語）"
         . ,(lambda ()
              (with-current-buffer split-buf (setq kill-ring nil))
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-post (split-pane-hwnd top) "down:30:24,up:30:24,down:30:24,up:30:24"))))
    (1.0 "ダブルクリックの結果 → トリプルクリック（行）"
         . ,(lambda ()
              (split-check "ダブルクリックで単語" (equal (car kill-ring) "line363") (format "%S" kill-ring))
              (with-current-buffer split-buf (setq kill-ring nil))
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-post (split-pane-hwnd top) "down:30:24,up:30:24,down:30:24,up:30:24,down:30:24,up:30:24"))))
    (1.0 "トリプルクリックの結果 → M-w で選択をコピーして消す"
         . ,(lambda ()
              (split-check "トリプルクリックで行" (equal (car kill-ring) "line363") (format "%S" kill-ring))
              (with-current-buffer split-buf (setq kill-ring nil))
              (split-type "" ?\M-w)))
    (0.8 "M-w の結果"
         . ,(lambda ()
              (split-check "M-w で選択がコピーされた" (equal (car kill-ring) "line363") (format "%S" kill-ring))
              (split-check "M-w で選択が消えた"
                           (null (with-current-buffer split-buf (embterm-module-selection embterm--term))))))
    (0.2 "ドラッグで選択（上のペインの 2 行目から 3 行目）"
         . ,(lambda ()
              (with-current-buffer split-buf (setq kill-ring nil))
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-post (split-pane-hwnd top)
                            "down:12:24,move:60:24,move:100:42,up:100:42"))))
    (1.0 "選択の確認（kill-ring に入る）"
         . ,(lambda ()
              (split-log "kill-ring: %S" kill-ring)
              (split-check "kill-ring に入った" (and kill-ring (string-match-p "line" (car kill-ring))))
              (split-check "クリップボードへも渡した（interprogram-cut-function）"
                           (and split-clipboard (string-match-p "line" split-clipboard))
                           (format "%S" split-clipboard))
              (split-check "module の選択のテキスト"
                           (with-current-buffer split-buf
                             (let ((s (embterm-module-selection embterm--term)))
                               (and s (string-match-p "line" s)))))
              (split-frame-png "frame-selection.png")
              (with-current-buffer split-buf
                (embterm-module-clear-selection embterm--term))
              ;; 上のペインをドラッグしたので上のウィンドウが選ばれている。下へ戻す（入力で上のスクロールが戻らないように）
              (split-check "クリックしたペインのウィンドウが選ばれた"
                           (eq (selected-window) (frame-first-window)))
              (select-window (next-window))))
    (0.5 "選択を消した"
         . ,(lambda ()
              (split-check "選択が消えた"
                           (null (with-current-buffer split-buf (embterm-module-selection embterm--term))))
              (kill-new "PASTED")
              (split-type "echo " ?\C-y 'return)))
    (2.0 "貼り付け"
         . ,(lambda ()
              (let* ((p (split-panes)) (bottom (nth 1 p)) (bt (split-pane-text bottom)))
                (split-check "貼り付けが端末に入った" (string-match-p "PASTED" (or bt "")) (format "%S" (seq-take (reverse (split-lines bt)) 4))))
              (select-window (frame-first-window))
              ;; 上のペインを選んで copy モード（遡った位置で入る）
              (split-type "" ?\C-c ?\C-k)))
    (1.5 "copy モード（遡った位置、スクロールバックが入る）"
         . ,(lambda ()
              (with-current-buffer split-buf
                (split-check "copy モードのテキストにスクロールバックの先頭"
                             (save-excursion
                               (goto-char (point-min))
                               (search-forward "line1" nil t)))
                (split-check "テキストに最新の行"
                             (save-excursion (goto-char (point-min)) (search-forward "more30" nil t)))
                (split-log "ポイントの行: %S" (buffer-substring-no-properties (line-beginning-position) (line-end-position)))
                (split-check "ポイントは遡っていた位置（line3xx の行）"
                             (string-match-p "\\`line[0-9]+" (buffer-substring-no-properties (line-beginning-position) (line-end-position)))))
              (split-frame-png "frame-copy.png")
              (split-type "" ?\C-c ?\C-j)))
    (1.5 "copy モードから戻った（ペインの位置はそのまま）"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-check "遡ったまま" (split-pane-scrolled top))
                (split-check "同じ行が見える" (equal (split-pane-text top) split-top-text)
                             (format "%S" (split-lines (split-pane-text top)))))
              (split-frame-png "frame-after-copy.png")
              (delete-other-windows)
              (split-window-right)))
    (1.5 "C-x 3（左右に分割。桁の違うペイン）"
         . ,(lambda ()
              (let ((p (split-panes)))
                (split-check "ペインが 2 つ" (= (length p) 2) (format "%S" p))
                (split-log "%s" (split-state)))
              (split-frame-png "frame-split-h.png")
              (other-window 1)))
    (1.5 "右を選ぶ（端末の大きさは右に合わせる。左は切り取って描く）"
         . ,(lambda ()
              (let* ((p (split-panes)) (left (nth 0 p)) (right (nth 1 p)))
                (split-log "%s" (split-state))
                ;; 端末の大きさは右のペインに合わせる（端末を選んだので変わる）。左のペインは遡ったまま
                (split-check "端末の大きさは右に合わせた"
                             (equal (split-pane-size right)
                                    (with-current-buffer split-buf
                                      (cons (plist-get embterm--state :cols) (plist-get embterm--state :rows))))
                             (split-state))
                (split-check "右のペインに最後の行" (string-match-p "PASTED" (or (split-pane-text right) "")))
                (split-check "左のペインは遡ったまま" (split-pane-scrolled left))
                (with-current-buffer split-buf
                  (split-check "カーソルは選んだペインのもの"
                               (consp (embterm-module-cursor embterm--term)) (format "%S" (embterm-module-cursor embterm--term)))))
              (split-frame-png "frame-split-h-right.png")
              (other-window 1)
              (enlarge-window-horizontally 20)))
    (1.5 "幅の違うペイン"
         . ,(lambda ()
              (split-log "%s" (split-state))
              (split-frame-png "frame-split-widths.png")
              ;; 左のペインはホイールを Emacs から受けて下へ戻す（Emacs 側に来たホイールの転送）
              (let ((left (frame-first-window)))
                (embterm-wheel (list 'wheel-down (posn-at-point (point-min) left)))
                (embterm-module-scroll embterm--term (embterm--pane-id (embterm--pane-of left)) "bottom" nil))))
    (0.8 "Emacs から転送したホイールとボトムへ戻す → 高さの違うペイン"
         . ,(lambda ()
              (split-check "左のペインは下へ戻った（遡っていない）" (not (split-pane-scrolled (nth 0 (split-panes)))))
              (delete-other-windows)
              (split-window-below)
              (other-window 1)
              (window-resize nil (- 8 (window-total-height)))))
    (1.5 "高さの違うペイン（端末は小さい下のペインに合わせ、大きい上はスクロールバックで埋める）"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)) (bottom (nth 1 p)))
                (split-log "%s" (split-state))
                (split-check "端末は小さいペインに合わせた"
                             (with-current-buffer split-buf
                               (eql (plist-get embterm--state :rows) (cdr (split-pane-size bottom)))))
                (split-check "大きいペインは遡っていない" (not (split-pane-scrolled top)))
                (let ((text (split-lines (split-pane-text top))))
                  (split-check "大きいペインの上はスクロールバック（行数が端末より多い）"
                               (> (length text) (cdr (split-pane-size bottom)))
                               (format "%d 行: %S" (length text) (seq-take text 4)))
                  (split-check "大きいペインの下の端に最新の行"
                               (string-match-p "PASTED" (mapconcat #'identity (last text 8) "\n"))
                               (format "%S" (last text 3)))))
              (split-frame-png "frame-split-small-bottom.png")
              (delete-other-windows)))
    (1.5 "C-x 1（ペインが 1 つに戻る）"
         . ,(lambda ()
              (split-check "ペインが 1 つ" (= (length (split-panes)) 1) (split-state))
              (split-frame-png "frame-one.png")
              (split-window-below)
              (other-window 1)
              (switch-to-buffer "*scratch*")))
    (1.0 "一方を別のバッファに（ペインは 1 枚に）"
         . ,(lambda ()
              (split-check "ペインが 1 つ" (= (length (split-panes)) 1) (split-state))
              (split-frame-png "frame-other-buffer.png")
              (delete-other-windows)
              (switch-to-buffer split-buf)
              (setq split-buf2 (let ((embterm-shell '("pwsh" "-NoLogo"))) (embterm-new)))))
    (4.0 "pwsh（ブラケットペーストのモード）に貼り付け、マウスの報告"
         . ,(lambda ()
              (with-current-buffer split-buf2
                (split-log "state=%S" (plist-get embterm--state :panes)))
              (setq embterm-confirm-unsafe-paste nil)
              (with-current-buffer split-buf2
                (split-log "貼り付けの確認: paste-safe-p=%S"
                           (embterm-module-paste-safe-p embterm--term "a\nb")))
              (kill-new "echo \"bp ok\"\necho second")
              (split-type "" ?\C-y 'return)))
    (2.0 "貼り付けの結果（改行を含む）"
         . ,(lambda ()
              (with-current-buffer split-buf2
                (let ((text (embterm-screen-text)))
                  (split-check "複数行の貼り付けが動いた" (string-match-p "bp ok" (or text "")) (format "%S" (seq-take-while (lambda (l) t) (split-lines text))))))
              (split-type "Write-Host -NoNewline \"$([char]27)[?1000h$([char]27)[?1006h\"" 'return)))
    (2.0 "マウスの報告が有効なときのホイール（スクロールしない）"
         . ,(lambda ()
              (let ((info (car (with-current-buffer split-buf2
                                 (embterm--poll-1)
                                 (plist-get embterm--state :panes)))))
                (split-post (nth 1 info) "wheel:360:50:50")
                (run-at-time 0.8 nil
                             (lambda ()
                               (let ((info2 (car (with-current-buffer split-buf2
                                                   (embterm--poll-1)
                                                   (plist-get embterm--state :panes)))))
                                 (split-check "遡らない（アプリへ送った）" (not (nth 4 info2)) (format "%S" info2))
                                 ;; マウスの報告が有効でも Shift+ドラッグは選択になる
                                 (setq kill-ring nil)
                                 (split-post (nth 1 info2)
                                             "sdown:12:24,smove:60:24,smove:100:24,sup:100:24")
                                 (run-at-time 0.7 nil
                                              (lambda ()
                                                (split-check "Shift+ドラッグは選択になる"
                                                             (and kill-ring (> (length (car kill-ring)) 0))
                                                             (format "%S" kill-ring))))))))))
    (1.5 "片付け"
         . ,(lambda ()
              (with-current-buffer split-buf2 (split-type "Write-Host -NoNewline \"$([char]27)[?1000l\"" 'return))
              (kill-buffer split-buf2)
              (switch-to-buffer split-buf)))
    (0.5 "小さなスクロールバックの上限（遡ったペインの行が消える）"
         . ,(lambda ()
              (customize-set-variable 'embterm-scrollback-lines 100)
              (split-window-below)
              (other-window 1)))
    (1.0 "上のペインを先頭まで遡る"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)))
                (with-current-buffer split-buf
                  (embterm-module-scroll embterm--term (car top) "top" nil)))))
    (0.8 "下のペインから出力（遡った行が消える）"
         . ,(lambda ()
              (split-log "上のペイン: %S" (seq-take (split-lines (split-pane-text (nth 0 (split-panes)))) 3))
              (with-current-buffer split-buf
                (embterm-module-send-string embterm--term "for /l %i in (1,1,800) do @echo flood%i\r"))))
    (4.0 "行が消えても壊れない（いちばん上へ寄る）"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-check "ペインは生きている" (and top (/= 0 (split-pane-hwnd top))) (format "%S" top))
                (split-check "遡ったまま（消えた行の代わりにいちばん古い行）" (split-pane-scrolled top))
                (split-log "上のペイン: %S" (seq-take (split-lines (split-pane-text top)) 3)))
              (split-frame-png "frame-pruned.png")
              (delete-other-windows)
              (with-current-buffer split-buf
                (embterm-module-send-string embterm--term "\r"))
              (split-frame-png "frame-final.png")))
    (1.0 "ドラッグをペインの上へ出したまま止める（自動スクロール。T-084）"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p)))
                (split-check "自動スクロールの前は下を追っている" (not (split-pane-scrolled top)))
                (split-post (split-pane-hwnd top) "down:30:60,move:30:-40,sleep:800,move:30:-40,up:30:-40"))))
    (1.0 "自動スクロールで遡った"
         . ,(lambda ()
              (let* ((p (split-panes)) (top (nth 0 p))
                     (sel (with-current-buffer split-buf (embterm-module-selection embterm--term))))
                (split-check "ペインが遡った" (split-pane-scrolled top) (format "%S" top))
                (split-check "選択が画面の上の外まで伸びた（10 行より多い）"
                             (and sel (> (length (split-string sel "\n")) 10))
                             (format "%S 行" (and sel (length (split-string sel "\n"))))))
              (with-current-buffer split-buf
                (embterm-module-clear-selection embterm--term))))
    (1.5 "終わり"
         . ,(lambda ()
              (split-log "NG の数: %d" split-ng)
              (kill-emacs (if (zerop split-ng) 0 3))))))

(run-at-time split-timeout nil (lambda () (split-log "時間切れで終える") (kill-emacs 2)))
(split-log "split.el を始める（Emacs %s）" emacs-version)
(split-run split-steps)

;;; split.el ends here
