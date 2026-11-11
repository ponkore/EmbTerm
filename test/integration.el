;;; integration.el --- Emacs との統合を別に起こした Emacs の中で確かめる（T-034 / T-035 / T-039 / T-040 / T-042 / T-080 / T-083） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/integration.el
;;
;; テーマの追従（PNG）、カーソルの形と点滅、OSC 52 / 9 / 777 / 9;4 / 8、ベルの間引き、複数の端末のコマンド、
;; バッファ名の規則、tr-ime のフックの差し替えを確かめる。結果は target/t034/ に書く:
;; integration.log（OK / NG）、frame-*.png（フレーム全体）、module.log。最後に Emacs を終える。
;; `sit-for' は使わない。ブラウザは開かない（`browse-url-browser-function' を差し替える）。

(require 'cl-lib)
(require 'seq)

(defconst it-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst it-out (expand-file-name "target/t034/" it-root))
(defconst it-log-file (expand-file-name "integration.log" it-out))
(defconst it-capture (expand-file-name "test/capture-window.ps1" it-root))
(defconst it-mouse (expand-file-name "test/post-mouse.ps1" it-root))
(defconst it-child (expand-file-name "test/osc-child.py" it-root))
(defconst it-timeout 200)
(defconst it-colors
  (concat (mapconcat (lambda (c) (format "\\e[%dm%d " c c)) (append (number-sequence 30 37) (number-sequence 90 97)) "")
          "\\e[0mplain")
  "osc-child.py の osc で出す 16 色の見本（\\e は osc-child.py が ESC に置き換える）。")

(make-directory it-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p it-log-file)
  (delete-file it-log-file))

(defvar it-ng 0 "NG の数。")
(defvar it-ok 0 "OK の数。")
(defvar it-a nil "最初の端末（cmd）。")
(defvar it-b nil)
(defvar it-p nil "プロジェクトの端末。")
(defvar it-mark nil "前の手順で覚えた値。")

(defun it-log (fmt &rest args)
  (let ((line (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))))
    (write-region line nil it-log-file t 'silent)))

(defun it-check (name ok &optional detail)
  (if ok (cl-incf it-ok) (cl-incf it-ng))
  (it-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun it-frame-png (name)
  (redisplay t)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              it-capture (frame-parameter nil 'window-id) (expand-file-name name it-out))))
    (it-log "フレームを保存: %s（%s）" name status)))

(defun it-type (string &rest keys)
  (setq unread-command-events (append unread-command-events (string-to-list string) keys)))

(defun it-run-child (&rest args)
  "端末（選んでいるウィンドウ）で osc-child.py を動かす。ARGS は 1 つずつ \"...\" で囲む。"
  (it-type (concat "python -I " it-child " "
                   (mapconcat (lambda (a) (format "\"%s\"" a)) args " "))
           'return))

(defun it-post (hwnd actions)
  (let ((status (call-process "powershell" nil nil nil "-NoProfile" "-ExecutionPolicy" "Bypass" "-File"
                              it-mouse (number-to-string hwnd) actions)))
    (it-log "マウス: %s（%s）" actions status)))

(defun it-info (&optional buf)
  (with-current-buffer (or buf it-a)
    (embterm-module-debug-info embterm--term)))

(defun it-hwnd (&optional buf)
  (with-current-buffer (or buf it-a)
    (embterm--poll-1)
    (plist-get embterm--state :hwnd)))

(defun it-file-text (file)
  (when (file-exists-p file)
    (with-temp-buffer
      (insert-file-contents file)
      (buffer-string))))

;;;; 記録用のフック

(defvar it-notes nil)
(defvar it-progress nil)
(defvar it-bells 0)
(defvar it-browsed nil)

(defun it-record-note (title body buffer)
  (push (list title body buffer) it-notes))

(defun it-record-progress (state percent buffer)
  (push (list state percent buffer) it-progress))

(defun it-record-browse (url &rest _)
  (push url it-browsed))

;;;; 単体の確認（module を使わないもの）

(defun it-check-buffer-names ()
  "T-080 の規則。"
  (with-temp-buffer
    (setq-local embterm--command "C:/Program Files/PowerShell/7/pwsh.exe")
    (it-check "名前: 長いパスのタイトルはファイル名だけ"
              (equal (embterm-default-buffer-name "C:\\WINDOWS\\SYSTEM32\\cmd.exe") "*embterm: cmd*")
              (embterm-default-buffer-name "C:\\WINDOWS\\SYSTEM32\\cmd.exe"))
    (it-check "名前: cmd がコマンドを動かしている間（パス - コマンド）"
              (equal (embterm-default-buffer-name "C:\\WINDOWS\\system32\\cmd.exe - python  x.py")
                     "*embterm: cmd - python  x.py*")
              (embterm-default-buffer-name "C:\\WINDOWS\\system32\\cmd.exe - python  x.py"))
    (it-check "名前: タイトルが無ければコマンド名"
              (equal (embterm-default-buffer-name nil) "*embterm: pwsh*")
              (embterm-default-buffer-name nil))
    (it-check "名前: パスでないタイトルはそのまま"
              (equal (embterm-default-buffer-name "✳ Claude Code") "*embterm: ✳ Claude Code*"))
    (let ((long (embterm-default-buffer-name (make-string 100 ?x))))
      (it-check "名前: 全体を 40 に収める" (<= (string-width long) 40) (format "%S %d" long (string-width long))))
    (let ((embterm-buffer-name-format nil))
      (it-check "名前: 書式が nil なら変えない" (null (embterm-default-buffer-name "abc"))))))

(defun it-check-tr-ime ()
  "T-083: tr-ime のフックの差し替え（tr-ime は emacs -Q に無いので、関数とフックを作って確かめる）。"
  (let ((calls nil) (global nil))
    ;; tr-ime が無いときは何もしない（変数を作らない）
    (with-temp-buffer
      (embterm-mode)
      (it-check "tr-ime: 無いときは何もしない"
                (and (not (boundp 'tr-ime-modadv--reconvertstring-hook))
                     (not (local-variable-p 'tr-ime-modadv--reconvertstring-hook)))))
    ;; tr-ime がある（advanced）ときの形を作る
    (defvar tr-ime-modadv--reconvertstring-hook nil)
    (defvar tr-ime-modadv--documentfeed-hook nil)
    (cl-letf (((symbol-function 'tr-ime-modadv--notify-reconvert-string)
               (lambda (hwnd str pos) (push (list hwnd str pos) calls))))
      (let ((g (lambda () (push (buffer-substring-no-properties (line-beginning-position) (line-end-position)) global))))
        (add-hook 'tr-ime-modadv--reconvertstring-hook g)
        (add-hook 'tr-ime-modadv--documentfeed-hook g)
        (with-temp-buffer
          (embterm-mode)
          (run-hooks 'tr-ime-modadv--reconvertstring-hook)
          (run-hooks 'tr-ime-modadv--documentfeed-hook)
          (it-check "tr-ime: 端末のバッファでは空の文字列を通知する"
                    (and (= (length calls) 2) (seq-every-p (lambda (c) (and (equal (nth 1 c) "") (eql (nth 2 c) 0))) calls))
                    (format "%S" calls))
          (it-check "tr-ime: 端末のバッファではグローバルのフック（ポイントの行を渡す）を呼ばない" (null global)
                    (format "%S" global))
          (it-check "tr-ime: 端末のバッファの中身は変わらない"
                    (string-prefix-p "EmbTerm:" (buffer-substring-no-properties (point-min) (point-max)))))
        (with-temp-buffer
          (insert "ふつうのバッファ")
          (run-hooks 'tr-ime-modadv--reconvertstring-hook)
          (it-check "tr-ime: ほかのバッファではグローバルのフックが動く" (equal global '("ふつうのバッファ"))
                    (format "%S" global)))
        (remove-hook 'tr-ime-modadv--reconvertstring-hook g)
        (remove-hook 'tr-ime-modadv--documentfeed-hook g)))
    (makunbound 'tr-ime-modadv--reconvertstring-hook)
    (makunbound 'tr-ime-modadv--documentfeed-hook)))

;;;; 手順

(defvar it-focus-faked nil "フレームにフォーカスが無いので `frame-focus-state' を差し替えたか。")

(defun it-fake-focus (on)
  (if on
      (unless (eq (frame-focus-state) t)
        (setq it-focus-faked t)
        (advice-add 'frame-focus-state :override #'always)
        (it-log "フレームにフォーカスが無いので frame-focus-state を t にした（点滅の確認のため）"))
    (when it-focus-faked
      (advice-remove 'frame-focus-state #'always)
      (setq it-focus-faked nil))))

(defun it-link-point (row col)
  "端末の ROW 行 COL 桁（0 から）のセルの中央の、子 HWND の中の画素。"
  (let* ((info (it-info))
         (cell (plist-get info :cell))
         (pad (plist-get info :padding)))
    (cons (+ pad (* col (car cell)) (/ (car cell) 2))
          (+ pad (* row (cdr cell)) (/ (cdr cell) 2)))))

(defun it-run (steps)
  (when steps
    (pcase-let ((`(,delay ,name . ,fn) (car steps)))
      (run-at-time delay nil
                   (lambda ()
                     (it-log "--- %s" name)
                     (condition-case err
                         (funcall fn)
                       (error (cl-incf it-ng) (it-log "NG エラー: %S" err)))
                     (it-run (cdr steps)))))))

(defconst it-steps
  `((1.0 "起動"
         . ,(lambda ()
              (set-frame-size (selected-frame) 120 40)
              (set-frame-position (selected-frame) 30 30)
              (load (expand-file-name "target/dist/embterm.el" it-root) nil t)
              (embterm--load-module)
              (embterm-module-set-log (expand-file-name "module.log" it-out))
              (it-check-buffer-names)
              (it-check-tr-ime)
              (setq embterm-shell '("cmd.exe"))
              (setq it-a (embterm-new))
              (it-check "作ってすぐコマンド名のバッファ名" (string-prefix-p "*embterm: cmd*" (buffer-name it-a))
                        (buffer-name it-a))))
    (3.0 "T-034: 既定のテーマ（emacs -Q は白地）に追従している"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-log "info: %S" info)
                (it-check "背景 = default の背景" (equal (plist-get info :background) (embterm--face-hex 'default :background))
                          (format "%s / %s" (plist-get info :background) (embterm--face-hex 'default :background)))
                (it-check "前景 = default の前景" (equal (plist-get info :foreground) (embterm--face-hex 'default :foreground))))
              (it-check "T-080: cmd のタイトル（exe のパス）はファイル名に" (string-prefix-p "*embterm: cmd" (buffer-name it-a))
                        (buffer-name it-a))
              (it-run-child "osc" it-colors)
              (it-frame-png "frame-theme-default.png")
              (load-theme 'modus-vivendi t)))
    (2.0 "modus-vivendi"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-log "info: %S" info)
                (it-check "vivendi: 背景が変わる" (equal (plist-get info :background) (embterm--face-hex 'default :background))
                          (format "%s / %s" (plist-get info :background) (embterm--face-hex 'default :background)))
                (it-check "vivendi: 赤 = ansi-color-red" (equal (plist-get info :palette1) (embterm--face-hex 'ansi-color-red :foreground))
                          (format "%s / %s" (plist-get info :palette1) (embterm--face-hex 'ansi-color-red :foreground))))
              (it-frame-png "frame-theme-vivendi.png")
              (it-run-child "osc" "\\e]11;#203040\\a")))
    (2.5 "アプリが OSC 11 で背景を変えた"
         . ,(lambda ()
              (it-check "OSC 11 の背景" (equal (plist-get (it-info) :background) "#203040") (plist-get (it-info) :background))
              (disable-theme 'modus-vivendi)
              (load-theme 'modus-operandi t)))
    (1.5 "modus-operandi（アプリの背景は残る）"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-check "テーマを替えてもアプリの背景は残る" (equal (plist-get info :background) "#203040") (plist-get info :background))
                (it-check "operandi: 前景は変わる" (equal (plist-get info :foreground) (embterm--face-hex 'default :foreground))
                          (format "%s / %s" (plist-get info :foreground) (embterm--face-hex 'default :foreground))))
              (it-run-child "osc" "\\e]111\\a")))
    (2.5 "OSC 111 で背景を戻す"
         . ,(lambda ()
              (it-check "operandi: 背景が戻る" (equal (plist-get (it-info) :background) (embterm--face-hex 'default :background))
                        (format "%s / %s" (plist-get (it-info) :background) (embterm--face-hex 'default :background)))
              (it-type "cls" 'return)
              (it-run-child "osc" it-colors)))
    (3.0 "operandi の画面"
         . ,(lambda ()
              (it-frame-png "frame-theme-operandi.png")
              (it-check "T-035: 既定はブロック" (eq (plist-get (it-info) :cursor-shape) 'block) (format "%S" (it-info)))
              (customize-set-variable 'embterm-cursor-style 'bar)))
    (1.0 "embterm-cursor-style = bar"
         . ,(lambda ()
              (it-check "bar になる" (eq (plist-get (it-info) :cursor-shape) 'bar) (format "%S" (plist-get (it-info) :cursor-shape)))
              (it-frame-png "frame-cursor-bar.png")
              (it-run-child "osc" "\\e[4 q")))
    (2.5 "アプリが DECSCUSR 4（下線）"
         . ,(lambda ()
              (it-check "アプリの形（下線）" (eq (plist-get (it-info) :cursor-shape) 'underline) (format "%S" (plist-get (it-info) :cursor-shape)))
              (customize-set-variable 'embterm-cursor-style 'block)))
    (1.0 "アプリが決めた後に設定を変える"
         . ,(lambda ()
              (it-check "アプリの形が優先（下線のまま）" (eq (plist-get (it-info) :cursor-shape) 'underline))
              (it-run-child "osc" "\\e[0 q")))
    (2.5 "アプリが DECSCUSR 0（既定へ戻す）"
         . ,(lambda ()
              (it-check "既定（ブロック）に戻る" (eq (plist-get (it-info) :cursor-shape) 'block) (format "%S" (plist-get (it-info) :cursor-shape)))
              (it-fake-focus t)
              (embterm--update-focus)
              (customize-set-variable 'embterm-cursor-blink t)))
    (1.0 "点滅を有効にした"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-log "info: %S" info)
                (it-check "点滅する" (plist-get info :cursor-blinking))
                (it-check "点滅のタイマーが動く" (plist-get info :blink-timer))
                (setq it-mark info))))
    (3.0 "3 秒点滅させる"
         . ,(lambda ()
              (let* ((info (it-info))
                     (ticks (- (plist-get info :blink-ticks) (plist-get it-mark :blink-ticks)))
                     (full (- (plist-get info :full-draws) (plist-get it-mark :full-draws)))
                     (partial (- (plist-get info :partial-draws) (plist-get it-mark :partial-draws))))
                (it-log "点滅 %d 回、全体を描いた %d 回、カーソルのセルだけ %d 回" ticks full partial)
                (it-check "点滅した" (>= ticks 4) (format "%d" ticks))
                (it-check "点滅はカーソルのセルだけを描く（全体は最初の 1〜2 回だけ）"
                          (and (<= full 2) (>= partial (- ticks 2))) (format "full %d partial %d ticks %d" full partial ticks)))
              (it-frame-png "frame-cursor-blink.png")
              (switch-to-buffer "*scratch*")))
    (1.0 "端末を隠した"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-check "隠すと点滅のタイマーが止まる" (not (plist-get info :blink-timer)) (format "%S" info))
                (setq it-mark info))))
    (1.0 "隠したまま"
         . ,(lambda ()
              (it-check "隠している間は点滅しない" (= (plist-get (it-info) :blink-ticks) (plist-get it-mark :blink-ticks)))
              (switch-to-buffer it-a)
              (customize-set-variable 'embterm-cursor-blink nil)))
    (1.0 "点滅を戻した"
         . ,(lambda ()
              (let ((info (it-info)))
                (it-check "点滅しない設定ではタイマーが止まる" (not (plist-get info :blink-timer)))
                (it-check "カーソルは点いている" (plist-get info :blink-on)))
              (it-fake-focus nil)
              (setq kill-ring nil)
              (it-run-child "clip-write" "embterm-osc52-write-1")))
    (2.5 "T-039: OSC 52 の書き込み"
         . ,(lambda ()
              (it-check "OSC 52 が kill-ring に入る" (equal (car kill-ring) "embterm-osc52-write-1") (format "%S" (car kill-ring)))
              (customize-set-variable 'embterm-allow-clipboard-write nil)
              (it-run-child "clip-write" "embterm-osc52-write-2")))
    (2.5 "書き込みを許さない"
         . ,(lambda ()
              (it-check "許さなければ kill-ring に入らない" (not (equal (car kill-ring) "embterm-osc52-write-2")) (format "%S" (car kill-ring)))
              (customize-set-variable 'embterm-allow-clipboard-write t)
              (it-run-child "clip-read" (expand-file-name "clip-read-default.log" it-out))))
    (3.5 "OSC 52 の読み取り（既定）"
         . ,(lambda ()
              (let ((text (it-file-text (expand-file-name "clip-read-default.log" it-out))))
                (it-check "既定では読み取りに答えない" (and text (not (string-match-p "52;" text))) (format "%S" text)))
              (customize-set-variable 'embterm-allow-clipboard-read t)
              (kill-new "embterm-clip-secret")
              (it-run-child "clip-read" (expand-file-name "clip-read-allowed.log" it-out))))
    (3.5 "OSC 52 の読み取り（許したとき）"
         . ,(lambda ()
              (let ((text (it-file-text (expand-file-name "clip-read-allowed.log" it-out))))
                (it-check "許すと答える（base64）" (and text (string-match-p (regexp-quote (base64-encode-string "embterm-clip-secret")) text))
                          (format "%S" text)))
              (customize-set-variable 'embterm-allow-clipboard-read nil)
              (add-hook 'embterm-notification-functions #'it-record-note)
              (it-run-child "notify")))
    (2.5 "OSC 9 / 777 の通知"
         . ,(lambda ()
              (let ((notes (reverse it-notes)))
                (it-log "通知: %S" notes)
                (it-check "OSC 9 で hook が呼ばれる" (equal (seq-take (car notes) 2) '("" "osc9 body text")))
                (it-check "OSC 777 で hook が呼ばれる" (equal (seq-take (cadr notes) 2) '("Claude Code" "Claude is waiting for your input")))
                (it-check "hook の BUFFER は端末" (eq (nth 2 (car notes)) it-a))
                (let ((text (embterm-notification-text "Claude Code" "waiting" it-a)))
                  (it-check "文面に出どころ（バッファ名）が入る" (string-match-p (regexp-quote (buffer-name it-a)) text) text))
                (with-current-buffer "*scratch*"
                  (let ((win (selected-window)))
                    (set-window-buffer win "*scratch*")
                    (embterm-notification-message "Claude Code" "waiting" it-a)
                    (it-check "端末を選んでいなければ message を出す" (string-match-p "embterm 通知" (or (current-message) ""))
                              (current-message))
                    (set-window-buffer win it-a))))
              (remove-hook 'embterm-notification-functions #'it-record-note)
              (add-hook 'embterm-progress-functions #'it-record-progress)
              (it-run-child "progress")))
    (2.5 "OSC 9;4 の進捗"
         . ,(lambda ()
              (let ((p (mapcar (lambda (e) (seq-take e 2)) (reverse it-progress))))
                (it-check "進捗の hook（set 50 → remove）" (and (member '(set 50) p) (member '(remove nil) p)) (format "%S" p)))
              (remove-hook 'embterm-progress-functions #'it-record-progress)
              (setq it-bells 0)
              (setq embterm-bell-function (lambda (&rest _) (cl-incf it-bells)))
              (it-run-child "bell" "100" "10")))
    (3.5 "T-040: ベルの連打（100 回、約 1 秒）"
         . ,(lambda ()
              (it-log "ベル: %d 回鳴らした" it-bells)
              (it-check "ベルは間引かれる（1〜8 回）" (<= 1 it-bells 8) (format "%d" it-bells))
              (setq embterm-bell-function #'ding)
              (setq browse-url-browser-function #'it-record-browse)
              (setq it-browsed nil)
              (it-run-child "link" "5" "5" "https://example.com/embterm-osc8?x=1")))
    (2.5 "T-039: OSC 8 のリンク"
         . ,(lambda ()
              (let* ((hwnd (it-hwnd))
                     (link (it-link-point 4 6))
                     (plain (it-link-point 4 (+ 4 15 4))))
                (with-current-buffer it-a
                  (let ((pane (embterm--pane-id (car embterm--panes))))
                    (it-check "リンクのセルの URI" (equal (embterm-module-link-at embterm--term pane (car link) (cdr link))
                                                         "https://example.com/embterm-osc8?x=1")
                              (format "%S at %S" (embterm-module-link-at embterm--term pane (car link) (cdr link)) link))
                    (it-check "リンクでないセル" (null (embterm-module-link-at embterm--term pane (car plain) (cdr plain))))))
                ;; Ctrl なしのクリック → 開かない。Ctrl+クリック → 開く
                (it-post hwnd (format "down:%d:%d,up:%d:%d" (car link) (cdr link) (car link) (cdr link)))
                (it-post hwnd (format "cdown:%d:%d,cup:%d:%d" (car link) (cdr link) (car link) (cdr link)))
                (it-post hwnd (format "cdown:%d:%d,cup:%d:%d" (car plain) (cdr plain) (car plain) (cdr plain))))))
    (1.5 "Ctrl+クリックの後"
         . ,(lambda ()
              (it-check "Ctrl+クリックで browse-url（1 回だけ）" (equal it-browsed '("https://example.com/embterm-osc8?x=1"))
                        (format "%S" it-browsed))
              (setq it-browsed nil)
              (embterm-open-hyperlink "javascript:alert(1)")
              (it-check "許していないスキームは開かない" (null it-browsed))
              (it-frame-png "frame-link.png")
              (setq browse-url-browser-function #'browse-url-default-browser)
              ;; T-042: 複数の端末
              (setq it-b (embterm-new))
              (it-check "embterm-new は新しく開く" (and (not (eq it-b it-a)) (= (length (embterm--live-buffers)) 2)))
              (let ((r (embterm)))
                (it-check "embterm は既存の（ほかの）端末へ切り替える" (and (eq r it-a) (= (length (embterm--live-buffers)) 2)
                                                                  (eq (window-buffer) it-a))
                          (format "%S" r)))
              (let ((c (embterm '(4))))
                (it-check "前置引数の embterm は新しく開く" (= (length (embterm--live-buffers)) 3))
                (kill-buffer c))
              (let ((default-directory (expand-file-name "test/" it-root)))
                (setq it-p (embterm-project)))
              (with-current-buffer it-p
                (it-check "embterm-project はプロジェクトのルートで開く"
                          (and (equal (file-truename default-directory) (file-truename (file-name-as-directory it-root)))
                               embterm--project)
                          (format "%s / %s" default-directory embterm--project)))
              (switch-to-buffer it-a)
              (let ((default-directory (expand-file-name "crates/" it-root)))
                (it-check "embterm-project は同じプロジェクトの端末を使い回す" (eq (embterm-project) it-p)))
              (switch-to-buffer it-b)
              (cl-letf (((symbol-function 'completing-read)
                         (lambda (_prompt collection &rest _)
                           (it-log "候補: %S" collection)
                           (buffer-name it-a))))
                (embterm-list-buffers))
              (it-check "embterm-list-buffers で選んだ端末へ" (eq (window-buffer) it-a))))
    (3.0 "バッファ名"
         . ,(lambda ()
              (it-log "バッファ: %S" (mapcar #'buffer-name (embterm--live-buffers)))
              (it-check "どの端末もコマンド名か短いタイトル（40 以下）"
                        (seq-every-p (lambda (b) (<= (string-width (buffer-name b)) 44)) (embterm--live-buffers)))
              (it-frame-png "frame-end.png")
              (kill-buffer it-b)
              (kill-buffer it-p)))
    (1.0 "終わり"
         . ,(lambda ()
              (it-log "OK の数: %d / NG の数: %d" it-ok it-ng)
              (kill-emacs (if (zerop it-ng) 0 3))))))

(run-at-time it-timeout nil (lambda () (it-log "時間切れで終える") (kill-emacs 2)))
(it-log "integration.el を始める（Emacs %s）" emacs-version)
(it-run it-steps)

;;; integration.el ends here
