;;; clip-nyagos.el --- nyagos の切り取りが Emacs から拾えるかの確認（#11） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**普段の Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   cargo xtask test-emacs clip
;;
;; target/dist/etc/embterm.lua を nyagos に読ませ、切り取り（C-k / C-u / C-w）が
;; Windows のクリップボードに入って Emacs から拾えることを見る
;; （[[file:../docs/notes/gh11-ck-cy.md][gh11-ck-cy.md]] の §6）。
;; 確かめること:
;;   1. rcfile 経由で `nyagos.option.clipboard' が立つ
;;   2. `C-k' で切り取った日本語がクリップボードに入り、`current-kill'（C-c C-y が通る道）が拾う
;;   3. 画面からは消え、切り取ったものは nyagos の中にも残る（端末の中の `C-y' で戻る）
;;   4. `C-u'（行頭からカーソルまで）と `C-w'（直前の語）も入る
;;   5. 画面で折り返す長い行でも欠けない
;;   6. 逆向き — Emacs でコピーしたものが端末の中の `C-y' で貼れる
;;   7. **起動した後に立てても効かない**（nyagos は rcfile を読み終えてから行編集の
;;      クリップボードを決める。embterm.lua の【重要】の裏付け）
;;
;; 利用者の ~/.nyagos ではなく、target/gh11/home に置いた .nyagos を HOME で読ませる
;; （nyagos は $HOME/.nyagos を読む。無ければ %USERPROFILE%）。
;; **クリップボードを書き換える**ので、始めに控えて終わりと時間切れで戻す。
;; コマンドは確定しないので nyagos の履歴は汚さない（`lua_e' の行だけは確定する）。
;; 合否は終了コード。結果は target/gh11/clip-nyagos.log。待ちは `run-at-time' で作る。

(require 'cl-lib)
(require 'seq)

(defconst o-root (or (getenv "EMBTERM_ROOT")
                     (expand-file-name ".." (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defconst o-out (expand-file-name "target/gh11/" o-root))
(defconst o-log (expand-file-name "clip-nyagos.log" o-out))
(defconst o-home (expand-file-name "home/" o-out))
(defconst o-lua (expand-file-name "target/dist/etc/embterm.lua" o-root))
(defconst o-long (concat "echo " (apply #'concat (make-list 40 "あいうえお1234567890")))
  "画面で折り返す長い行（605 文字）。")
(defvar o-failures nil)
(defvar o-buf nil)
(defvar o-saved-clip nil)

(make-directory o-home t)
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

(add-to-list 'load-path (expand-file-name "target/dist" o-root))
(require 'embterm)
(set-frame-size nil 120 36)

(defun o-send (s) (with-current-buffer o-buf (embterm-send-string s)))
(defun o-screen () (with-current-buffer o-buf (embterm-screen-text)))
(defun o-line ()
  "画面の最後の空でない行（いま編集している行）。プロンプトの右に出るので前後の空白は落とす。"
  (or (car (last (seq-remove #'string-empty-p
                             (mapcar #'string-trim (split-string (o-screen) "\n")))))
      ""))
(defun o-clip () (ignore-errors (gui-get-selection 'CLIPBOARD)))
(defun o-set-clip (s) (ignore-errors (gui-set-selection 'CLIPBOARD s)))
(defun o-restore () (o-set-clip (or o-saved-clip "")))

(defun o-start (rc)
  "RC（nil なら置かない）を HOME の .nyagos として置き、nyagos の端末を開く。"
  (let ((f (expand-file-name ".nyagos" o-home)))
    (if rc (write-region rc nil f nil 'silent) (when (file-exists-p f) (delete-file f))))
  (let ((embterm-shell '("nyagos"))
        (process-environment (cons (format "HOME=%s" (directory-file-name o-home)) process-environment)))
    (setq o-buf (embterm-new)))
  (delete-other-windows))

(defun o-kill-buf ()
  (let ((kill-buffer-query-functions nil))
    (when (buffer-live-p o-buf) (kill-buffer o-buf))))

(defun o-run (steps)
  (when steps
    (run-at-time (caar steps) nil
                 (lambda ()
                   (condition-case err (funcall (cdar steps))
                     (error (o-check nil "手順で error: %S" err)))
                   (o-run (cdr steps))))))

(setq o-saved-clip (o-clip))
(o-log "元のクリップボードの長さ: %S" (length o-saved-clip))

(o-run
 `((0.5 . ,(lambda ()
             ;; 本番と同じ経路（~/.nyagos から dofile）で読ませる
             (o-start (format "dofile([[%s]])\n" o-lua))))
   (4.0 . ,(lambda ()
             (o-send "lua_e \"print(nyagos.option.clipboard)\"\r")))
   (2.0 . ,(lambda ()
             (o-log "起動直後の画面:\n%s" (string-trim (o-screen)))
             (o-check (string-match-p "^true$" (o-screen))
                      "rcfile 経由で nyagos.option.clipboard が立った")
             (o-set-clip "EMBTERM-BEFORE")
             (o-send "echo あいうえおかきくけこ")))
   ;; 2. C-k（「お」と「か」の間へ戻してから）
   (1.5 . ,(lambda () (dotimes (_ 5) (o-send "\C-b"))))
   (1.0 . ,(lambda () (o-send "\C-k")))
   (1.5 . ,(lambda ()
             (o-check (equal (o-clip) "かきくけこ")
                      "C-k で切り取った「かきくけこ」がクリップボードに入った（%S）" (o-clip))
             (o-check (equal (o-line) "echo あいうえお") "画面からは消えた（%S）" (o-line))
             ;; C-c C-y が通る道。`current-kill' は拾ったものを kill-ring に積むので、先に空にしておく
             (setq kill-ring nil kill-ring-yank-pointer nil)
             (o-check (equal (current-kill 0) "かきくけこ")
                      "Emacs の current-kill（C-c C-y）が拾う（%S）" (car kill-ring))
             ;; 3. nyagos の中にも残っている
             (o-send "\C-y")))
   (1.5 . ,(lambda ()
             (o-check (equal (o-line) "echo あいうえおかきくけこ")
                      "端末の中の C-y で戻る（切り取ったものが nyagos の中にも残っている。%S）" (o-line))
             ;; 4. C-u（行末にカーソルがあるので行全体）
             (o-send "\C-u")))
   (1.5 . ,(lambda ()
             (o-check (equal (o-clip) "echo あいうえおかきくけこ")
                      "C-u で行頭からカーソルまでが入った（%S）" (o-clip))
             (o-send "echo あい うえ おか")))
   (1.5 . ,(lambda () (o-send "\C-w")))
   (1.5 . ,(lambda ()
             (o-check (equal (o-clip) "おか") "C-w で直前の語が入った（%S）" (o-clip))
             (o-send "\C-u")))
   ;; 5. 画面で折り返す長い行
   (1.2 . ,(lambda () (o-send o-long)))
   (2.5 . ,(lambda () (o-send "\C-a")))
   (1.5 . ,(lambda () (o-send "\C-k")))
   (2.0 . ,(lambda ()
             (o-check (equal (o-clip) o-long)
                      "折り返す長い行（%d 文字）が欠けずに入った（%d 文字）"
                      (length o-long) (length (or (o-clip) "")))
             ;; 6. 逆向き（Emacs でコピーしたものを端末の中の C-y で貼る）
             (o-set-clip "EMACSからの貼り付け")))
   (1.5 . ,(lambda () (o-send "\C-y")))
   (1.5 . ,(lambda ()
             (o-check (equal (o-line) "EMACSからの貼り付け")
                      "Emacs でコピーしたものが端末の中の C-y で貼れる（%S）" (o-line))
             (o-send "\C-u")
             (o-kill-buf)
             ;; 7. 起動した後に立てても効かない（.nyagos を置かずに起こす）
             (o-start nil)))
   (4.0 . ,(lambda ()
             (o-send "lua_e \"nyagos.option.clipboard = true print(nyagos.option.clipboard)\"\r")))
   (2.0 . ,(lambda ()
             (o-check (string-match-p "^true$" (o-screen)) "（後から）値そのものは立つ")
             (o-set-clip "EMBTERM-NOT-CHANGED")
             (o-send "echo あとから")))
   (1.5 . ,(lambda () (o-send "\C-a")))
   (1.0 . ,(lambda () (o-send "\C-k")))
   (1.5 . ,(lambda ()
             ;; 立っていれば「echo あとから」が入るはず。入らなければ効いていない
             ;; （Emacs が直前に置いた値が残るか、取れずに nil になるかは問わない）
             (o-check (not (equal (o-clip) "echo あとから"))
                      "起動した後に立てても効かない（LazySetup は rcfile の後。%S）" (o-clip))
             (o-check (not (string-match-p "あとから" (o-screen)))
                      "そのときも C-k そのものは効く（最後の行は %S）" (o-line))
             (o-send "\C-u")
             (o-kill-buf)
             (o-restore)
             (o-log "終わり（NG %d 件）" (length o-failures))
             (kill-emacs (if o-failures 1 0))))))

(run-at-time 150 nil (lambda () (o-check nil "時間切れ") (o-log "時間切れ") (o-restore) (kill-emacs 2)))

;;; clip-nyagos.el ends here
