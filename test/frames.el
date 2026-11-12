;;; frames.el --- Emacs 側の操作との組み合わせ（T-044）の自動確認 -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/frames.el
;;
;; 結果は target/t044/frames.log（OK / NG）と、フレーム全体の画面（frame-*.png。PrintWindow で子ウィンドウ・子フレームも入る）。
;; 確かめること:
;;   - tab-bar: タブのバーが出ると端末が下にずれる。別のタブに移ると端末は見えなくなり、戻ると見える（その間の出力も出る）
;;   - 子フレーム（corfu などのポップアップ）: 端末の上に出る。端末が描き直しても隠れない
;;   - 最小化: 最小化している間の出力が、戻したときに出ている
;;   - フレームの削除: 端末を出していたフレームを消しても端末とプロセスは残り、別のフレームに出し直せる
;; 画面は目で確かめる（ログに PNG の名前を出す）。`sit-for' は使わない

(require 'cl-lib)
(require 'seq)

(defconst frames-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst frames-out (expand-file-name "target/t044/" frames-root))
(defconst frames-log-file (expand-file-name "frames.log" frames-out))

(make-directory frames-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p frames-log-file) (delete-file frames-log-file))

(defvar frames-ng 0)
(defvar frames-buf nil)
(defvar frames-child nil)
(defvar frames-frame2 nil)
(defvar frames-pid nil)

(defun frames-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil frames-log-file t 'silent)))

(defun frames-check (name ok &optional detail)
  (unless ok (cl-incf frames-ng))
  (frames-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun frames-ps (script &rest args)
  (with-temp-buffer
    (apply #'call-process "powershell" nil t nil "-NoProfile" "-ExecutionPolicy" "Bypass"
           "-File" (expand-file-name script frames-root) args)
    (string-trim (buffer-string))))

(defun frames-capture (name &optional frame)
  (redisplay t)
  (frames-log "フレームを保存: %s" (frames-ps "test/capture-window.ps1" (frame-parameter (or frame (selected-frame)) 'window-id)
                                         (expand-file-name name frames-out))))

(defun frames-panes ()
  "端末のペイン（module の :panes）。`(ID HWND COLS ROWS SCROLLED)' のリスト。"
  (plist-get (buffer-local-value 'embterm--state frames-buf) :panes))

(defun frames-pane-info ()
  "ペインの子 HWND の (見えているか 左 上 右 下) のリスト（HWND の無いペインは nil）。"
  (mapcar (lambda (p)
            (let ((hwnd (nth 1 p)))
              (and (integerp hwnd) (> hwnd 0)
                   (let ((f (split-string (frames-ps "test/window-info.ps1" (number-to-string hwnd)))))
                     (and (= (length f) 6) (mapcar #'string-to-number (cdr f)))))))
          (frames-panes)))

(defun frames-check-placed (label)
  "見えているペインが 1 つで、端末のウィンドウの本文の位置にあるか（隠して取っておくペインは数えない。T-087）。"
  (let* ((win (get-buffer-window frames-buf))
         (edges (and win (window-inside-pixel-edges win)))
         (info (frames-pane-info))
         (visible (seq-filter (lambda (i) (and i (= (nth 0 i) 1))) info)))
    (frames-check (format "%s: 見えているペインは 1 つで、ウィンドウの本文の位置" label)
                  (and edges (= (length visible) 1)
                       (equal (cdr (car visible)) edges))
                  (format "ペイン %S / 本文 %S" info edges))))

(defun frames-shown-pane ()
  "端末のウィンドウのペイン（module の :panes の項目）。"
  (let* ((win (get-buffer-window frames-buf))
         (p (and win (with-current-buffer frames-buf (embterm--pane-of win)))))
    (and p (assq (embterm--pane-id p) (frames-panes)))))

(defvar frames-scrolled-id nil)

(defun frames-check-hidden (label)
  (let ((info (frames-pane-info)))
    (frames-check (format "%s: 端末は見えない" label)
                  (seq-every-p (lambda (i) (or (null i) (= (nth 0 i) 0))) info)
                  (format "%S" info))))

(defun frames-send (s) (with-current-buffer frames-buf (embterm-send-string s)))

(defun frames-screen-has (re)
  (string-match-p re (with-current-buffer frames-buf (or (embterm-screen-text) ""))))

(defun frames-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (frames-log "--- %s" name)
                     (condition-case err (funcall fn)
                       (error (frames-check (format "エラーなし（%s）" name) nil (format "%S" err))))
                     (frames-run (cdr steps)))))))

(defconst frames-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 110 34)
              (set-frame-position (selected-frame) 60 60)
              (load (expand-file-name "target/dist/embterm.el" frames-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module-frames.log" frames-out))
              (let ((embterm-shell '("cmd.exe")))
                (setq frames-buf (embterm-new)))
              (delete-other-windows)))
    (2.5 "最初"
         . ,(lambda ()
              (setq frames-pid (plist-get (buffer-local-value 'embterm--state frames-buf) :pid))
              (frames-check-placed "最初")
              (frames-send "echo first\r")
              (tab-bar-mode 1)))
    (1.0 "tab-bar-mode でタブのバーが出た"
         . ,(lambda ()
              (frames-check-placed "タブのバーの下へずれた")
              (frames-capture "frame-1-tabbar.png")
              (tab-bar-new-tab)
              (switch-to-buffer "*scratch*")))
    (1.0 "別のタブ（*scratch*）"
         . ,(lambda ()
              (frames-check-hidden "別のタブ")
              (frames-capture "frame-2-other-tab.png")
              (frames-send "echo while-other-tab\r")))
    (1.0 "元のタブへ戻る"
         . ,(lambda () (tab-bar-switch-to-prev-tab)))
    (1.0 "元のタブ"
         . ,(lambda ()
              (frames-check-placed "元のタブ")
              (frames-check "別のタブにいた間の出力がある" (frames-screen-has "^while-other-tab"))
              (frames-capture "frame-3-back-tab.png")
              ;; 子フレーム（corfu のポップアップと同じ形）を端末の上に出す
              (let ((popup (get-buffer-create "*popup*")))
                (with-current-buffer popup
                  (erase-buffer)
                  (insert "child frame 1\nchild frame 2\nchild frame 3\n子フレーム 4\n")
                  (setq mode-line-format nil))
                (setq frames-child
                      (make-frame `((parent-frame . ,(selected-frame)) (minibuffer . nil) (no-accept-focus . t)
                                    (undecorated . t) (left . 120) (top . 120) (width . 30) (height . 5)
                                    (visibility . t) (background-color . "light yellow"))))
                (set-window-buffer (frame-root-window frames-child) popup))))
    (1.0 "子フレームを出した"
         . ,(lambda ()
              (frames-capture "frame-4-childframe.png")
              (frames-send "echo redraw-under-child\r")))
    (1.0 "子フレームの下で端末が描き直した"
         . ,(lambda ()
              (frames-capture "frame-5-childframe-after-output.png")
              (delete-frame frames-child)
              (iconify-frame)))
    (1.0 "最小化した"
         . ,(lambda ()
              (frames-log "フレームの状態: %S" (frame-visible-p (selected-frame)))
              (frames-send "echo while-iconified\r")))
    (1.5 "最小化から戻す"
         . ,(lambda ()
              (make-frame-visible)
              (raise-frame)))
    (1.0 "戻した"
         . ,(lambda ()
              (frames-check-placed "最小化から戻した")
              (frames-check "最小化していた間の出力がある" (frames-screen-has "^while-iconified"))
              (frames-capture "frame-6-deiconified.png")
              ;; もう 1 つのフレームに端末を出し、元のフレームは *scratch* にする
              (switch-to-buffer "*scratch*")
              (setq frames-frame2 (make-frame '((width . 90) (height . 25) (left . 300) (top . 200))))
              (select-frame frames-frame2)
              (switch-to-buffer frames-buf)))
    (1.5 "別のフレームに出した"
         . ,(lambda ()
              (frames-check-placed "別のフレーム")
              (frames-capture "frame-7-frame2.png" frames-frame2)
              (delete-frame frames-frame2)))
    (1.0 "そのフレームを消した"
         . ,(lambda ()
              (frames-check "端末のバッファは残る" (buffer-live-p frames-buf))
              (frames-check "プロセスは残る" (and frames-pid (process-attributes frames-pid)))
              (frames-send "echo after-delete-frame\r")
              (switch-to-buffer frames-buf)))
    (1.5 "元のフレームに出し直した"
         . ,(lambda ()
              (frames-check-placed "元のフレームに出し直した")
              (frames-check "消した後の出力がある" (frames-screen-has "^after-delete-frame"))
              (frames-capture "frame-8-after-delete.png")
              (frames-send "for /l %i in (1,1,80) do @echo n%i\r")))
    (2.0 "スクロールバックを遡る（T-087）"
         . ,(lambda ()
              (let ((p (frames-shown-pane)))
                (setq frames-scrolled-id (car p))
                (with-current-buffer frames-buf
                  (embterm-module-scroll embterm--term (car p) "lines" -20)))))
    (1.0 "遡った → 別のタブへ"
         . ,(lambda ()
              (frames-check "遡った" (nth 4 (frames-shown-pane)) (format "%S" (frames-shown-pane)))
              (tab-bar-switch-to-next-tab)))
    (1.0 "別のタブ → 戻る"
         . ,(lambda ()
              (frames-check-hidden "別のタブ（2 回目）")
              (tab-bar-switch-to-prev-tab)))
    (1.0 "タブを行き来した後"
         . ,(lambda ()
              (frames-check-placed "タブを行き来した後")
              (let ((p (frames-shown-pane)))
                (frames-check "同じペインで、遡ったまま" (and p (eql (car p) frames-scrolled-id) (nth 4 p))
                              (format "%S（前のペイン %S）" p frames-scrolled-id)))
              ;; 右に *scratch* を出して、左右のウィンドウを入れ替える（端末は右のウィンドウへ移る）
              (delete-other-windows)
              (set-window-buffer (split-window-right) (get-buffer-create "*scratch*"))
              (window-swap-states (frame-first-window) (next-window (frame-first-window)))))
    (1.0 "window-swap-states の後"
         . ,(lambda ()
              (frames-check-placed "window-swap-states の後")
              (let ((p (frames-shown-pane)))
                (frames-check "端末は右のウィンドウ" (eq (get-buffer-window frames-buf) (next-window (frame-first-window))))
                (frames-check "入れ替えた後も遡ったまま" (and p (nth 4 p)) (format "%S" p)))
              (frames-capture "frame-9-swapped.png")
              (frames-log "失敗の数: %d" frames-ng)
              (kill-emacs (if (zerop frames-ng) 0 1))))))

(run-at-time 90 nil (lambda () (frames-log "時間切れで終える") (kill-emacs 2)))
(frames-log "frames.el を始める（Emacs %s）" emacs-version)
(frames-run frames-steps)

;;; frames.el ends here
