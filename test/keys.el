;;; keys.el --- EmbTerm のキー入力の自動確認（T-069 / T-070 / T-071 / T-074） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/keys.el
;;
;; target/dist の embterm.el と module を読み、test/key-child.py（console / vt / vt-kkp / query / syncout / drop-kkp）を
;; 端末の中で順に動かして、キーを `unread-command-events' から送る。結果は target/t069/ に書く:
;;   sent.log     送ったキー（時刻 名前）
;;   child-*.log  子が受け取ったもの（時刻 内容）
;;   merged.txt   時刻で並べたもの（`keys--finish' が作る）
;;   keys.log     手順の記録
;; 最後に Emacs を終える（時間の上限 `keys-timeout' 秒でも終える）。`sit-for' は使わない。

(require 'cl-lib)
(require 'seq)

(defconst keys-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst keys-out (expand-file-name "target/t069/" keys-root))
(defconst keys-timeout 150 "この秒数が経ったら Emacs を終える。")
(defconst keys-child (expand-file-name "test/key-child.py" keys-root))
(defconst keys-python (or (executable-find "python") "python"))

(make-directory keys-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(dolist (f (directory-files keys-out t "\\.\\(log\\|txt\\)\\'"))
  (delete-file f))

(defun keys-out (name) (expand-file-name name keys-out))
(defconst keys-sent-log (keys-out "sent.log"))

(defun keys-log (fmt &rest args)
  (write-region (concat (format-time-string "%H:%M:%S.%3N ") (apply #'format fmt args) "\n")
                nil (keys-out "keys.log") t 'silent))

(defun keys-sent (name)
  (write-region (format "%.3f SEND %s\n" (float-time) name) nil keys-sent-log t 'silent))

(defconst keys-events
  '("C-SPC" "C-@" "S-<return>" "C-<return>" "RET" "M-a" "M-<return>" "<f5>" "C-<f5>" "S-<f5>" "<up>" "C-<left>"
    "<tab>" "<backtab>" "S-<tab>" "<escape>" "<backspace>" "C-<backspace>" "a" "A" "1" "!" "C-a" "C-S-a" "C-2"
    "<kp-1>" "<kp-add>" "<kp-enter>" "<home>" "<deletechar>" "あ" "😀" "※" "C-あ" "M-あ")
  "送るキー（`kbd' の形）。")

(defun keys-spawn (mode)
  "子プロセス key-child.py を MODE で起動して、端末のバッファを返す。"
  (let ((embterm-shell (list keys-python keys-child mode (keys-out (format "child-%s.log" mode)) "120")))
    (embterm)))

(defun keys-send-1 (name)
  (keys-sent name)
  (setq unread-command-events (append unread-command-events (listify-key-sequence (kbd name)))))

(defvar keys-buffer nil)

(defun keys-sequence (mode)
  "MODE の子を起動してキーを順に送る手順（`keys-run' の項目）のリスト。"
  (append
   (list
    `(1.0 ,(format "%s: 起動" mode)
          . ,(lambda ()
               (setq keys-buffer (keys-spawn mode))
               (keys-sent (format "---- %s" mode)))))
   (let ((first t) steps)
     (dolist (ev keys-events)
       (push `(,(if first 1.5 0.25) ,(format "%s: %s" mode ev)
               . ,(lambda () (keys-send-1 ev)))
             steps)
       (setq first nil))
     (nreverse steps))
   (list
    `(0.5 ,(format "%s: 閉じる" mode)
          . ,(lambda ()
               (when (buffer-live-p keys-buffer)
                 (with-current-buffer keys-buffer
                   (write-region (or (embterm-screen-text) "") nil (keys-out (format "screen-%s.txt" mode)) nil 'silent))
                 (kill-buffer keys-buffer)))))))

(defun keys-query-steps ()
  (list
   `(1.0 "query: 起動"
         . ,(lambda ()
              (setq keys-buffer (keys-spawn "query"))
              (keys-sent "---- query")))
   `(3.0 "query: 閉じる"
         . ,(lambda ()
              (when (buffer-live-p keys-buffer)
                (with-current-buffer keys-buffer
                  (write-region (or (embterm-screen-text) "") nil (keys-out "screen-query.txt") nil 'silent))
                (kill-buffer keys-buffer))))))

(defvar keys-sync-samples nil)

(defun keys-sync-steps ()
  "同期出力: hold の間、ペインの表示（描画の写し）に partial の途中が見えないことを確かめる。"
  (list
   `(1.0 "syncout: 起動"
         . ,(lambda ()
              (setq keys-buffer (keys-spawn "syncout"))
              (keys-sent "---- syncout")
              (setq keys-sync-samples nil)
              (let ((n 0) timer)
                (setq timer
                      (run-at-time
                       0.25 0.03
                       (lambda ()
                         (setq n (1+ n))
                         (when (> n 70) (cancel-timer timer))
                         (when (buffer-live-p keys-buffer)
                           (with-current-buffer keys-buffer
                             (let* ((pane (car embterm--panes))
                                    (text (and pane (embterm-module-pane-text embterm--term (embterm--pane-id pane)))))
                               (when text
                                 (push (cons (float-time) text) keys-sync-samples)))))))))))
   `(3.0 "syncout: 結果"
         . ,(lambda ()
              (let ((partial-seen 0) (ok t))
                (dolist (s (reverse keys-sync-samples))
                  (when (string-match-p "partial-" (cdr s))
                    (setq partial-seen (1+ partial-seen))
                    (unless (string-match-p "partial-9" (cdr s))
                      (setq ok nil)
                      (keys-log "途中の partial が見えた: %S" (cdr s)))))
                (keys-log "syncout: サンプル %d、partial が見えたサンプル %d、途中が見えなかった %S"
                          (length keys-sync-samples) partial-seen ok))
              (when (buffer-live-p keys-buffer)
                (kill-buffer keys-buffer))))))

(defun keys-kkp-drop-steps ()
  "KKP を求めた子が CSI <u を出さずに落ちたあと、コンソールのアプリに戻ったときにゴミ文字が入らないこと。"
  (list
   `(1.0 "drop-kkp: pwsh を起動"
         . ,(lambda ()
              (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
                (setq keys-buffer (embterm)))
              (keys-sent "---- drop-kkp")))
   `(3.0 "drop-kkp: 子を起動して CSI >5u のまま落とす"
         . ,(lambda ()
              (keys-sent "drop-kkp: python start")
              (setq unread-command-events
                    (append (string-to-list
                             (format "& '%s' '%s' drop-kkp '%s' 0.1"
                                     (subst-char-in-string ?/ ?\\ keys-python)
                                     (subst-char-in-string ?/ ?\\ keys-child)
                                     (keys-out "child-drop.log")))
                            '(return)))))
   `(4.0 "drop-kkp: KKP のフラグが残っていても、pwsh にゴミ文字が入らない"
         . ,(lambda ()
              (keys-log "子が落ちたあとの送り方: %S"
                        (with-current-buffer keys-buffer (embterm-module-key-protocol embterm--term)))
              (keys-sent "drop-kkp: echo at prompt")
              ;; KKP のまま C-SPC を送るとゴミ文字（CSI 32;5u の文字）になる。安全策が効けばならない
              (setq unread-command-events (append (listify-key-sequence (kbd "C-SPC"))
                                                  (string-to-list "echo KKP-ok") '(return)))))
   `(2.0 "drop-kkp: 画面を保存して閉じる"
         . ,(lambda ()
              (keys-log "キーを送ったあとの送り方: %S"
                        (with-current-buffer keys-buffer (embterm-module-key-protocol embterm--term)))
              (with-current-buffer keys-buffer
                (write-region (or (embterm-screen-text) "") nil (keys-out "screen-drop.txt") nil 'silent))
              (kill-buffer keys-buffer)))))

(defun keys-mouse-wheel-steps ()
  "マウスの報告（子 HWND への PostMessage）と代替画面のホイール（矢印キーになる）。"
  (let ((post (lambda (buf actions)
                (let ((hwnd (plist-get (buffer-local-value 'embterm--state buf) :hwnd)))
                  (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass"
                                "-File" (expand-file-name "test/post-mouse.ps1" keys-root)
                                (number-to-string hwnd) actions)))))
    (list
     `(1.0 "mouse: 起動"
           . ,(lambda ()
                (setq keys-buffer (keys-spawn "mouse"))
                (keys-sent "---- mouse")))
     `(1.5 "mouse: クリックとドラッグ"
           . ,(lambda ()
                (keys-sent "mouse: click 3,2 and drag to 12,4")
                (funcall post keys-buffer "down:30:30,up:30:30,sleep:50,down:30:30,move:60:40,move:110:80,up:110:80")))
     `(1.5 "mouse: 閉じる"
           . ,(lambda () (kill-buffer keys-buffer)))
     `(1.0 "altwheel: 起動"
           . ,(lambda ()
                (setq keys-buffer (keys-spawn "altwheel"))
                (keys-sent "---- altwheel")))
     `(1.5 "altwheel: ホイール（上へ 1 ノッチ、下へ 2 ノッチ）"
           . ,(lambda ()
                (keys-sent "altwheel: wheel up 1 notch, down 2 notches")
                (funcall post keys-buffer "wheel:120:30:30,sleep:100,wheel:-240:30:30")))
     `(1.5 "altwheel: 閉じる"
           . ,(lambda () (kill-buffer keys-buffer))))))

(defun keys-pwsh-steps ()
  "PSReadLine が C-SPC を受けること（Ctrl+Spacebar のハンドラが呼ばれたらファイルに書く）。"
  (list
   `(1.0 "pwsh: 起動"
         . ,(lambda ()
              (let ((embterm-shell '("pwsh" "-NoLogo" "-NoProfile")))
                (setq keys-buffer (embterm)))
              (keys-sent "---- pwsh")))
   `(3.0 "pwsh: ハンドラを登録"
         . ,(lambda ()
              (setq unread-command-events
                    (append (string-to-list
                             (format "Set-PSReadLineKeyHandler -Chord 'Ctrl+Spacebar' -ScriptBlock { Add-Content -LiteralPath '%s' 'ctrl-space-handled' }"
                                     (subst-char-in-string ?/ ?\\ (keys-out "psrl.log"))))
                            '(return)))))
   `(1.5 "pwsh: C-SPC を送る"
         . ,(lambda () (keys-send-1 "C-SPC")))
   `(0.8 "pwsh: echo（pwsh が生きていることの確認。IME の確定文字と絵文字も通す）"
         . ,(lambda ()
              (setq unread-command-events (append (string-to-list "echo psrl-alive-あ😀※") '(return)))))
   `(2.0 "pwsh: 画面を保存して閉じる"
         . ,(lambda ()
              (with-current-buffer keys-buffer
                (write-region (or (embterm-screen-text) "") nil (keys-out "screen-pwsh.txt") nil 'silent))
              (keys-log "psrl.log の有無: %S" (file-exists-p (keys-out "psrl.log")))
              (kill-buffer keys-buffer)))))

(defun keys-vim-steps ()
  "vim（`:set mouse=a'）: クリックで位置が動くこと。VT の代替画面の表示。"
  (let ((vim (or (executable-find "C:/Users/masao/scoop/shims/vim.exe") "vim")))
    (list
     `(1.0 "vim: 起動"
           . ,(lambda ()
                (let ((embterm-shell (list vim "-u" "NONE" "-N" "-c" "set mouse=a" "--" (expand-file-name "docs/TODO.md" keys-root))))
                  (setq keys-buffer (embterm)))
                (keys-sent "---- vim")))
     `(3.0 "vim: クリック（桁 10、行 5。0 始まり）"
           . ,(lambda ()
                (keys-sent "vim: click cell (10,5)")
                (keys-screenshot "vim-before.png")
                (let* ((st (buffer-local-value 'embterm--state keys-buffer))
                       (cell (keys-module-cell keys-buffer))
                       (x (+ 4 (* 10 (car cell))))
                       (y (+ (/ (cdr cell) 2) (* 5 (cdr cell)))))
                  (keys-log "state %S cell %S click %d,%d" (plist-get st :hwnd) cell x y)
                  (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass"
                                "-File" (expand-file-name "test/post-mouse.ps1" keys-root)
                                (number-to-string (plist-get st :hwnd))
                                (format "down:%d:%d,up:%d:%d" x y x y)))))
     `(1.0 "vim: 位置を書き出す"
           . ,(lambda ()
                (setq unread-command-events
                      (append (string-to-list
                               (format ":call writefile([string(getpos('.'))], '%s')"
                                       (keys-out "vim-pos.log")))
                              '(return)))))
     `(1.5 "vim: 終了"
           . ,(lambda ()
                (keys-screenshot "vim-after.png")
                (setq unread-command-events (append (string-to-list ":q!") '(return)))))
     `(1.5 "vim: 閉じる"
           . ,(lambda ()
                (when (buffer-live-p keys-buffer)
                  (kill-buffer keys-buffer))
                (keys-log "vim-pos.log: %s"
                          (when (file-exists-p (keys-out "vim-pos.log"))
                            (with-temp-buffer (insert-file-contents (keys-out "vim-pos.log")) (buffer-string)))))))))

(defun keys-screenshot (name)
  (when (buffer-live-p keys-buffer)
    (with-current-buffer keys-buffer
      (embterm-screenshot (keys-out name)))))

(defun keys-module-cell (buf)
  "ペインのセルの大きさ (幅 . 高さ)。"
  (with-current-buffer buf
    (let* ((rect (embterm-module-cursor embterm--term)))
      (if rect (cons (nth 2 rect) (nth 3 rect)) (cons 8 18)))))

(defconst keys-groups
  `(("mouse" . keys-mouse-wheel-steps)
    ("console" . ,(lambda () (keys-sequence "console")))
    ("vt" . ,(lambda () (keys-sequence "vt")))
    ("kkp" . ,(lambda () (keys-sequence "vt-kkp")))
    ("query" . keys-query-steps)
    ("sync" . keys-sync-steps)
    ("drop" . keys-kkp-drop-steps)
    ("vim" . keys-vim-steps)
    ("pwsh" . keys-pwsh-steps)))

(defun keys--finish ()
  (let (lines)
    (dolist (f (directory-files keys-out t "\\.log\\'"))
      (unless (equal (file-name-nondirectory f) "keys.log")
        (with-temp-buffer
          (insert-file-contents f)
          (dolist (l (split-string (buffer-string) "\n" t))
            (push (concat l "  [" (file-name-nondirectory f) "]") lines)))))
    (with-temp-file (keys-out "merged.txt")
      ;; 同じ時刻の行は元の順のまま（時刻の部分だけで比べる）
      (dolist (l (seq-sort-by (lambda (l) (substring l 0 (min 14 (length l)))) #'string< (nreverse lines)))
        (insert l "\n"))))
  (keys-log "終える")
  (kill-emacs 0))

(defun keys-run (steps)
  (if (null steps)
      (keys--finish)
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (keys-log "--- %s" name)
                     (condition-case err
                         (funcall fn)
                       (error (keys-log "エラー: %S" err)))
                     (keys-run (cdr steps)))))))

(run-at-time keys-timeout nil (lambda () (keys-log "時間切れで終える") (kill-emacs 2)))
(keys-log "keys.el を始める（Emacs %s）" emacs-version)
(load (expand-file-name "target/dist/embterm.el" keys-root) nil t)
(embterm--load-module)
(embterm-module-set-log (keys-out "module.log"))
(setq embterm-kill-buffer-on-exit nil)
;; 環境変数 KEYS_ONLY に「drop,vim」のように並べると、その組だけ走らせる
(let ((only (getenv "KEYS_ONLY")))
  (keys-run (apply #'append
                   (mapcar (lambda (g) (funcall (cdr g)))
                           (if only
                               (seq-filter (lambda (g) (member (car g) (split-string only ",")))
                                           keys-groups)
                             keys-groups)))))

;;; keys.el ends here
