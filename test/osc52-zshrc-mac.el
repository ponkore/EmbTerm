;;; osc52-zshrc-mac.el --- 利用者の ~/.zshrc の OSC 52 の設定を切り分ける -*- lexical-binding: t; -*-

;; **利用者の ~/.zshrc に依存するので、`cargo xtask test-emacs' には登録していない。**
;; 手で動かす（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   EMBTERM_ROOT=$PWD emacs -Q -l $PWD/test/osc52-zshrc-mac.el
;;
;; ~/.zshrc に OSC 52 の設定（[[file:../docs/notes/gh11-ck-cy.md][gh11-ck-cy.md]] の §5.1）を
;; 入れたときに、本当に効いているか・zsh 本来の挙動を壊していないかを見る。
;; **合否は付けない**（環境ごとに結果が変わるため）。観測した結果を
;; target/gh11/osc52-zshrc.log に書くので、それを読む。
;;
;; 誰の環境でも同じになるはずのこと（受け取る側の長さ、ウィジェットの包み方の比較）は
;; osc52-mac.el のほうで合否を付けている。
;;
;; 2026-11-19 に masao の環境（zim + zsh-autosuggestions + zsh-syntax-highlighting）で
;; 見たときは、こうだった:
;;   - `C-k' で切り取ったものは kill-ring に入った（設定は効いている）
;;   - 続けて kill したときの `$CUTBUFFER' への追記は**失われていた**。原因は
;;     zsh-autosuggestions が `precmd' で後からウィジェットを包み直すラッパーで、
;;     こちらのラッパーを抜いても（②）追記されない。ビルトインに戻すと（③）追記される。
;;     つまり EmbTerm 側の設定とは無関係
;;
;; ② の `zle -A .kill-word autosuggest-orig-1-kill-word' は zsh-autosuggestions が
;; 元のウィジェットを退避する名前に依っている。入れていない環境では失敗するので、
;; その場合は ① と ③ だけを見る。

(require 'cl-lib)
(require 'seq)

(defconst o-root (or (getenv "EMBTERM_ROOT")
                     (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst o-out (expand-file-name "target/gh11/" o-root))
(defconst o-log (expand-file-name "osc52-zshrc.log" o-out))

(make-directory o-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p o-log) (delete-file o-log))

(defun o-log (fmt &rest args)
  (write-region (format "%s\n" (apply #'format fmt args)) nil o-log t 'silent))

(setq embterm-module-directory (expand-file-name "target/dist" o-root))
(load (expand-file-name "target/dist/embterm.el" o-root) nil t)
;; 利用者の設定どおり、$SHELL をログインシェルで起こす（~/.zshrc が読まれる）
(setq embterm-shell (list (or (getenv "SHELL") "/bin/zsh") "-l"))
(setq embterm-poll-interval 0.05)

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
                                (error (o-log "手順で error: %S" e) (o-finish)))))
    (o-finish)))

(defun o-finish ()
  (o-log "終わり（合否は付けない。上の観測を読む）")
  (kill-emacs 0))

;;;; 見方
;;
;; A. `C-k' で切り取ったものが kill-ring に入るか（OSC 52 の設定が効いているか）
;; B. "aaa bbb ccc" で C-a → M-d → M-d → C-e → C-y。追記が効いていれば `aaa bbb' が
;;    貼られ、効いていなければ ` bbb' だけが貼られる

(defun o-trial (name setup)
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
     (let ((line (or (car (last (o-lines))) "")))
       (o-log "    貼り付け後の行: %S" line)
       (o-log "    追記: %s" (cond ((string-match-p "ccc *aaa bbb" line) "あり")
                                   ((string-match-p "ccc *bbb" line) "なし")
                                   (t "判定できず")))
       (o-log "    kill-ring（OSC 52 が来たか）: %S" kill-ring))
     (o-send "\C-u")
     (o-next 0.5))))

(setq o-steps
      (append
       (list
        (lambda ()
          (embterm--load-module)
          (setq o-buf (embterm))
          (redisplay t)
          (o-next 4.0))   ; プロンプトの初期化（zim・starship など）を待つ
        (lambda ()
          ;; 誰が kill 系のウィジェットを持っているか
          (o-send "clear; zle -lL | grep -i 'kill-word' | head -8; echo \"HL=${ZSH_HIGHLIGHT_HIGHLIGHTERS:-none}\"; echo \"AS=${ZSH_AUTOSUGGEST_STRATEGY:-none}\"\r")
          (o-next 1.2))
        (lambda ()
          (o-log "=== kill-word をめぐるウィジェットと、ハイライト・補完の有無 ===")
          (dolist (l (o-lines)) (o-log "    %s" l))
          (o-next 0.3))

        ;; A. C-k が OSC 52 に乗るか
        (lambda ()
          (o-log "--- A. C-k で切り取ったものが kill-ring に入るか")
          (setq kill-ring nil kill-ring-yank-pointer nil)
          (o-send "clear\r")
          (o-next 0.5))
        (lambda () (o-send "あいうえおかきくけこ") (o-next 0.8))
        (lambda () (dotimes (_ 5) (o-send "\C-b")) (o-next 0.6))
        (lambda () (o-send "\C-k") (o-next 1.2))
        (lambda ()
          (o-log "    C-k の後の画面:\n%s" (string-trim (o-text)))
          (o-log "    kill-ring: %S（\"かきくけこ\" なら効いている）" kill-ring)
          (o-send "\C-u")
          (o-next 0.6)))

       ;; B. 続けて kill したときの追記を、ラップの構成を変えて見る
       (o-trial "B① いまの ~/.zshrc のまま" nil)
       (o-trial "B② こちらのラッパーだけ抜く（補完のラッパー → ビルトイン）"
                "zle -A .kill-word autosuggest-orig-1-kill-word")
       (o-trial "B③ 両方抜く（ビルトイン直）" "zle -A .kill-word kill-word")

       (list
        (lambda ()
          ;; コマンドは確定していないので履歴は汚れない。バッファごと閉じる
          (let ((kill-buffer-query-functions nil))
            (when (buffer-live-p o-buf) (kill-buffer o-buf)))
          (o-next 0.5)))))

(run-at-time 1.0 nil #'o-next)
(run-at-time 240 nil (lambda () (o-log "時間切れ") (o-finish)))
