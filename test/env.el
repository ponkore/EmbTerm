;;; env.el --- 子の環境変数と `embterm-run' の自動確認（T-109 / T-110） -*- lexical-binding: t; -*-

;; 使い方（リポジトリの直下で。**ユーザーが使っている Emacs には読み込まない**）:
;;
;;   cargo xtask dist
;;   ET_REMOVE=from-os C:/Apps/emacs/emacs-31.1/bin/emacs.exe -Q -l test/env.el
;;
;; ET_REMOVE は Emacs の起動時の環境に置いておく（削除の確認に使う。無くても他の項目は確かめる）。
;; 確かめること:
;;   - `setenv' した変数が `embterm-new' の子に届く（`make-process' と同じ）
;;   - `embterm-run' の ENVIRONMENT で、起動時の環境にある変数を消せる・非 ASCII の値を渡せる
;;   - `process-environment' の TERM は端末の値（xterm-256color）に負ける。PWD は子に渡さない
;;   - NAME のバッファ名がタイトルで付け替わらない、DISPLAY の関数で出る、作業ディレクトリ
;; 結果は target/t109/env.log（OK / NG）。最後に Emacs を終える。
;; 注意: `sit-for' は使わない（待ちはすべて `run-at-time' の間隔で作る）

(require 'cl-lib)
(require 'seq)

(defconst env-root (expand-file-name ".." (file-name-directory (or load-file-name buffer-file-name))))
(defconst env-out (expand-file-name "target/t109/" env-root))
(defconst env-log-file (expand-file-name "env.log" env-out))
(defvar env-ng 0)

(make-directory env-out t)
(prefer-coding-system 'utf-8)
(setq coding-system-for-write 'utf-8)
(when (file-exists-p env-log-file)
  (delete-file env-log-file))

(defun env-log (fmt &rest args)
  (write-region (format "[%s] %s\n" (format-time-string "%H:%M:%S.%3N") (apply #'format fmt args))
                nil env-log-file t 'silent))

(defun env-check (name ok &optional detail)
  (unless ok (setq env-ng (1+ env-ng)))
  (env-log "%s %s%s" (if ok "OK" "NG") name (if detail (format " -- %s" detail) "")))

(defun env-text (buf)
  (if (buffer-live-p buf)
      (with-current-buffer buf (or (embterm-screen-text) ""))
    ""))

(add-to-list 'load-path (expand-file-name "target/dist" env-root))
(require 'embterm)
(set-frame-size nil 120 40)

;; 子が環境を表示して待つコマンド（cmd の %VAR% は未定義ならそのまま残る）
(defconst env-echo
  '("cmd.exe" "/d" "/k" "echo" "L=[%LANG%]" "R=[%ET_REMOVE%]" "X=[%ET_X%]" "T=[%TERM%]" "D=[%CD%]" "P=[%PWD%]" "W=[%WEZTERM_PANE%]"))

(defvar env-buf1 nil)
(defvar env-buf2 nil)
(defvar env-buf3 nil)
(defvar env-had-remove (getenv-internal "ET_REMOVE" initial-environment))

(defun env-step1 ()
  (env-log "起動時の ET_REMOVE: %S" env-had-remove)
  ;; 1. setenv が embterm-new の子に届く（TERM=dumb は端末の値に負ける）
  (setenv "LANG" "ja_JP.UTF-8")
  (setenv "TERM" "dumb")
  (setenv "PWD" "C:/Windows")
  (let ((embterm-shell env-echo)
        (default-directory (file-name-as-directory env-root)))
    (setq env-buf1 (embterm-new)))
  ;; 2. embterm-run: ET_REMOVE を消し、ET_X に非 ASCII、NAME と DISPLAY と DIRECTORY を指定
  (let ((process-environment (copy-sequence process-environment))
        (shown nil))
    (setenv "ET_REMOVE" nil)
    (setenv "ET_X" "あ①")
    (setq env-buf2 (embterm-run env-echo
                                :directory (expand-file-name "target" env-root)
                                :environment process-environment
                                :name "*env-test*"
                                :display (lambda (b) (setq shown t) (pop-to-buffer b))))
    (env-check "DISPLAY の関数で出した" shown)
    (env-check "embterm-run がバッファを返す" (buffer-live-p env-buf2) env-buf2))
  ;; 3. 出さずに起動する（DISPLAY が ignore。T-082）
  (setq env-buf3 (embterm-run '("cmd.exe" "/d" "/k" "echo" "BG=[started]") :name "*env-bg*" :display #'ignore))
  (env-check "出さずに開いたバッファはどのウィンドウにも無い" (null (get-buffer-window env-buf3 t)))
  ;; 呼び出しの外の process-environment は変わっていない
  (env-check "呼び出しの外では ET_X が無い" (null (getenv "ET_X"))))

(defun env-step2 ()
  (let ((t1 (env-text env-buf1))
        (t2 (env-text env-buf2)))
    (write-region t1 nil (expand-file-name "new.txt" env-out) nil 'silent)
    (write-region t2 nil (expand-file-name "run.txt" env-out) nil 'silent)
    (env-log "embterm-new の画面:\n%s" (string-trim t1))
    (env-log "embterm-run の画面:\n%s" (string-trim t2))
    (env-check "setenv の LANG が子に届く" (string-search "L=[ja_JP.UTF-8]" t1))
    (env-check "TERM は端末の値" (string-search "T=[xterm-256color]" t1))
    (env-check "PWD は子に渡さない" (string-search "P=[%PWD%]" t1))
    (if (getenv-internal "WEZTERM_PANE" initial-environment)
        (env-check "親の端末の変数（WEZTERM_PANE）は子に渡さない（T-095）" (string-search "W=[%WEZTERM_PANE%]" t1))
      (env-log "WEZTERM_PANE が無い（WezTerm の外で起動した）ので T-095 の確認は飛ばす"))
    (when env-had-remove
      (env-check "embterm-new の子には ET_REMOVE が残る" (string-search (format "R=[%s]" env-had-remove) t1))
      (env-check "embterm-run で ET_REMOVE を消せる" (string-search "R=[%ET_REMOVE%]" t2)))
    (env-check "embterm-new の子に ET_X は無い" (string-search "X=[%ET_X%]" t1))
    (env-check "embterm-run で非 ASCII の値を渡せる" (string-search "X=[あ①]" t2))
    (env-check "embterm-run の LANG（呼び出し時の process-environment）" (string-search "L=[ja_JP.UTF-8]" t2))
    (env-check "DIRECTORY で起動した" (string-search "\\target]" t2))
    (env-check "作業ディレクトリのドライブレターは大文字" (string-match-p "D=\\[[A-Z]:" t1))
    (env-check "NAME のバッファ名のまま" (equal (buffer-name env-buf2) "*env-test*") (buffer-name env-buf2))
    (env-check "embterm-new はタイトルで名前が付く" (not (equal (buffer-name env-buf1) "*embterm*"))
               (buffer-name env-buf1))
    ;; 出さずに起動したもの: 表示を待たずに動き、80x24 で起動している
    (let ((st (buffer-local-value 'embterm--state env-buf3)))
      (env-check "出さずに起動した（:started）" (plist-get st :started) (format "%S" (plist-get st :pid)))
      (env-check "出さずに起動した大きさは 80x24" (and (eql (plist-get st :cols) 80) (eql (plist-get st :rows) 24))
                 (format "%Sx%S" (plist-get st :cols) (plist-get st :rows)))
      (env-check "出さずに起動したものの出力" (string-search "BG=[started]" (env-text env-buf3))))
    ;; 後で出すとウィンドウの大きさに合わせる（次の手順で見る）
    (switch-to-buffer env-buf3)))

(defun env-step3 ()
  (let ((st (buffer-local-value 'embterm--state env-buf3)))
    (env-check "出したらウィンドウの大きさに合わせた" (not (eql (plist-get st :cols) 80))
               (format "%Sx%S" (plist-get st :cols) (plist-get st :rows)))))

(defun env-finish ()
  (env-log "NG の数: %d" env-ng)
  (let ((kill-buffer-query-functions nil))
    (dolist (b (list env-buf1 env-buf2 env-buf3))
      (when (buffer-live-p b) (kill-buffer b))))
  (kill-emacs (if (zerop env-ng) 0 1)))

(defun env-guard (fn)
  (lambda ()
    (condition-case err
        (funcall fn)
      (error (env-check (format "%s でエラー" fn) nil (format "%S" err))
             (env-finish)))))

(run-at-time 0.5 nil (env-guard #'env-step1))
(run-at-time 5 nil (env-guard #'env-step2))
(run-at-time 6.5 nil (env-guard #'env-step3))
(run-at-time 7 nil (env-guard #'env-finish))
(run-at-time 40 nil (lambda () (env-log "時間切れ") (kill-emacs 2)))
