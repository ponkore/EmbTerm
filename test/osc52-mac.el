;;; osc52-mac.el --- OSC 52 でアプリから kill-ring へ渡せるかの確認（#11 の §5） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   cargo xtask test-emacs osc52
;;
;; 確かめること:
;;   1. 受け取る側 — OSC 52 で送ったテキストが、長さを問わず kill-ring に入る
;;      （`embterm-allow-clipboard-write' は既定 t。T-039）
;;   2. 出す側 — zsh の ZLE の kill 系ウィジェットを包めば、`C-k' で切り取ったものを
;;      OSC 52 で渡せる（[[file:../docs/notes/gh11-ck-cy.md][gh11-ck-cy.md]] の §5.1）
;;   3. 包んだときに、zsh 本来の「続けて kill すると `$CUTBUFFER' へ追記」が
;;      失われないか。`zle -f kill' の有無と呼ぶ位置で比べる
;;
;; シェルは zsh -f（利用者の ~/.zshrc を読まない）。**利用者の環境での切り分けは
;; osc52-zshrc-mac.el のほう**（あちらは環境に依存するので xtask に登録していない）。
;; 合否は終了コード。結果は target/gh11/osc52.log。待ちは `run-at-time' で作る。

(require 'cl-lib)
(require 'seq)

(defconst o-root (or (getenv "EMBTERM_ROOT")
                     (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst o-out (expand-file-name "target/gh11/" o-root))
(defconst o-log (expand-file-name "osc52.log" o-out))
(defvar o-failures nil)

(make-directory o-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p o-log) (delete-file o-log))

(defun o-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))
                nil o-log t 'silent))

(defun o-check (ok fmt &rest args)
  (let ((msg (apply #'format fmt args)))
    (o-log "%s %s" (if ok "OK" "NG") msg)
    (unless ok (push msg o-failures))
    ok))

(setq embterm-module-directory (expand-file-name "target/dist" o-root))
(load (expand-file-name "target/dist/embterm.el" o-root) nil t)
(setq embterm-shell '("/bin/zsh" "-f"))
(setq embterm-poll-interval 0.05)

(defconst o-sizes '(1000 4000 16000 64000 256000)
  "OSC 52 で送ってみるテキストの長さ。libghostty-vt の OSC のバッファは 2048 だが、
OSC 52 はそれを超えられる作りになっている（notes の §5.2）。")

(defvar o-buf nil)
(defvar o-steps nil)

(defun o-send (s)
  (with-current-buffer o-buf (embterm-module-send-string embterm--term s)))
(defun o-text ()
  (with-current-buffer o-buf (or (ignore-errors (embterm-module-text embterm--term)) "")))
(defun o-lines ()
  (seq-remove #'string-empty-p (mapcar #'string-trim (split-string (o-text) "\n"))))

(defun o-next (&optional delay)
  (if-let* ((step (pop o-steps)))
      (run-at-time (or delay 0.9) nil
                   (lambda () (condition-case e (funcall step)
                                (error (o-check nil "手順で error: %S" e) (o-finish)))))
    (o-finish)))

(defun o-finish ()
  (o-log "終わり（NG %d 件）" (length o-failures))
  (kill-emacs (if o-failures 1 0)))

;;;; 出す側の比べ方
;;
;; "aaa bbb ccc" を打ち、C-a で行頭、M-d を 2 回（kill-word）、C-e で行末、C-y で zsh 自身の
;; yank。追記が効いていれば `aaa bbb' が貼られ、効いていなければ ` bbb' だけが貼られる。
;; zsh の kill-word は単語の末尾までを切るので、2 回目は先頭に空白が付く。

(defun o-trial (name setup append-expected)
  "SETUP でウィジェットを差し替えてから、続けて kill したときの追記を見る手順を返す。
APPEND-EXPECTED が non-nil なら追記されることを期待する。"
  (list
   (lambda ()
     (setq kill-ring nil kill-ring-yank-pointer nil)
     (o-log "--- %s" name)
     (if setup (progn (o-send (concat setup "\r")) (o-next 0.6)) (o-next 0.2)))
   (lambda () (o-send "clear\r") (o-next 0.5))
   (lambda () (o-send "aaa bbb ccc") (o-next 0.7))
   (lambda () (o-send "\C-a") (o-next 0.4))
   (lambda () (o-send "\ed") (o-next 0.6))
   (lambda () (o-send "\ed") (o-next 0.6))
   (lambda () (o-send "\C-e") (o-next 0.4))
   (lambda () (o-send "\C-y") (o-next 0.8))
   (lambda ()
     (let* ((line (or (car (last (o-lines))) ""))
            (appended (and (string-match-p "ccc *aaa bbb" line) t)))
       (o-log "    貼り付け後の行: %S" line)
       (o-log "    kill-ring: %S" kill-ring)
       (o-check (eq appended (and append-expected t))
                "%s: 続けて kill したときの追記が %s（行 %S）"
                name (if append-expected "ある" "ない") line))
     (o-send "\C-u")
     (o-next 0.5))))

(setq o-steps
      (append
       (list
        (lambda ()
          (embterm--load-module)
          (embterm-module-set-log (expand-file-name "osc52-module.log" o-out))
          (setq o-buf (embterm))
          (redisplay t)
          (o-next 1.5))
        (lambda ()
          (let ((s (with-current-buffer o-buf (embterm-module-poll embterm--term nil))))
            (o-check (plist-get s :started) "zsh が起動した（pid %s）" (plist-get s :pid)))
          (o-next 0.3)))

       ;; 1. 受け取る側: 長さを変えて OSC 52 を直に出す
       (apply #'append
              (mapcar (lambda (n)
                        (list
                         (lambda ()
                           (setq kill-ring nil kill-ring-yank-pointer nil)
                           (o-send (format "clear; printf '\\033]52;c;%%s\\a' \"$(head -c %d /dev/zero | tr '\\0' a | base64 | tr -d '\\n')\"\r" n))
                           (o-next 1.2))
                         (lambda ()
                           (let* ((got (car kill-ring)) (len (length (or got ""))))
                             (o-check (and (= len n) (string-match-p "\\`a+\\'" (or got "")))
                                      "OSC 52 で送った %d 文字が kill-ring に入った（%d 文字）" n len))
                           (o-next 0.3))))
                      o-sizes))

       ;; 2. 出す側: ZLE のウィジェットを包んで C-k を OSC 52 に乗せる
       (list
        (lambda ()
          (o-send "bindkey -e\r")
          (o-next 0.6))
        (lambda ()
          ;; 切り取った $CUTBUFFER を base64 にして OSC 52 で出す（notes の §5.1 と同じ形）
          (o-send "_c() { [[ -n $1 ]] || return 0; printf '\\033]52;c;%s\\a' \"$(printf '%s' \"$1\" | base64 | tr -d '\\n')\" }\r")
          (o-next 0.5))
        (lambda ()
          (o-send "_k() { zle .kill-line; zle -f kill; _c \"$CUTBUFFER\" }\r")
          (o-next 0.4))
        (lambda ()
          (o-send "zle -N kill-line _k\r")
          (o-next 0.4))
        (lambda ()
          (o-send "clear\r")
          (o-next 0.5))
        (lambda ()
          (setq kill-ring nil kill-ring-yank-pointer nil)
          (o-send "あいうえおかきくけこ")
          (o-next 0.8))
        (lambda ()
          ;; 「お」と「か」の間へ
          (dotimes (_ 5) (o-send "\C-b"))
          (o-next 0.6))
        (lambda ()
          (o-send "\C-k")
          (o-next 1.2))
        (lambda ()
          (o-log "C-k の後の画面:\n%s" (string-trim (o-text)))
          (o-check (equal (car kill-ring) "かきくけこ")
                   "zsh が C-k で切り取ったものが OSC 52 で kill-ring に入った（%S）" (car kill-ring))
          (o-send "\C-u")
          (o-next 0.6))

        ;; 3. 続けて kill したときの追記。比べるための 3 つのラッパーを足す
        (lambda ()
          (o-send "_w_after() { zle .kill-word; zle -f kill; _c \"$CUTBUFFER\" }\r")
          (o-next 0.4))
        (lambda ()
          (o-send "_w_before() { zle -f kill; zle .kill-word; _c \"$CUTBUFFER\" }\r")
          (o-next 0.4))
        (lambda ()
          (o-send "_w_none() { zle .kill-word; _c \"$CUTBUFFER\" }\r")
          (o-next 0.4)))

       (o-trial "① 包まない（素の zsh）" nil t)
       (o-trial "② 包んで zle -f kill を後に呼ぶ（notes の §5.1 の形）" "zle -N kill-word _w_after" t)
       (o-trial "③ 包んで zle -f kill を前に呼ぶ" "zle -N kill-word _w_before" t)
       (o-trial "④ 包んで zle -f kill を呼ばない（追記が失われる）" "zle -N kill-word _w_none" nil)

       (list
        (lambda ()
          ;; コマンドは確定していないので履歴は汚れない。バッファごと閉じる
          (let ((kill-buffer-query-functions nil))
            (when (buffer-live-p o-buf) (kill-buffer o-buf)))
          (o-check (not (buffer-live-p o-buf)) "バッファを消した")
          (o-next 0.5)))))

(run-at-time 1.0 nil #'o-next)
(run-at-time 240 nil (lambda () (o-check nil "時間切れ") (o-finish)))
