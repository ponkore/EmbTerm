;;; probe.el --- macOS への移植の可否を確かめるプローブ  -*- lexical-binding: t -*-

;; 使い方: ./run.sh （GUI の Emacs が一瞬立ち上がり、result.txt を書いて終わる）
;;
;; 確かめること（docs/research/macos-port.md §3）
;;   P1 Emacs の NS フレームのビューの階層と、そこに子 NSView を足せるか
;;   P2 Emacs の再描画で子ビューが消えないか
;;   P3 Lisp が止まっている間も別スレッドの描画（CATransaction の commit）が進むか
;;   P4 マウス（hitTest）が子ビューに当たるか
;;   P5 child frame（corfu / posframe）との重なり
;;   P6 フレーム → NSWindow の対応付け（window-id は使えるか。EmacsView の ivar）

(defvar probe-dir
  ;; GUI の Emacs では default-directory が当てにならないので、run.sh が渡す PROBE_DIR を優先する
  (file-name-as-directory
   (or (getenv "PROBE_DIR")
       (file-name-directory (expand-file-name (or load-file-name buffer-file-name))))))
(defvar probe-out (expand-file-name "result.txt" probe-dir))

(module-load (expand-file-name "spike.dylib" probe-dir))
(module-load (expand-file-name "ivar.dylib" probe-dir))

(defun probe-out (fmt &rest args)
  (write-region (concat (apply #'format fmt args) "\n") nil probe-out 'append))

(write-region "" nil probe-out)

(run-at-time
 1 nil
 (lambda ()
   (condition-case err
       (progn
         (probe-out "* 環境")
         (probe-out "emacs-version=%s system-configuration=%s" emacs-version system-configuration)
         (probe-out "module-file-suffix=%S window-system=%S" module-file-suffix (window-system))

         ;; P1 / P2: 子ビューを足し、Emacs 側の描画を起こしてから色を読む
         (probe-out "\n* P1 / P2 埋め込みと Emacs の再描画")
         (dotimes (i 200) (insert (format "line %d: Emacs の描画が重なるかの確認 abcdefghij\n" i)))
         (goto-char (point-min))
         (redisplay t)
         (let* ((e (window-inside-pixel-edges))
                (x (nth 0 e)) (y (nth 1 e))
                (w (- (nth 2 e) x)) (h (- (nth 3 e) y)))
           (probe-out "window-inside-pixel-edges=%S frame-pixel=%dx%d" e (frame-pixel-width) (frame-pixel-height))
           (probe-out "attach => %S" (spike-attach (frame-parameter nil 'name) (+ x 20) (+ y 20) (- w 40) (- h 40))))
         (dotimes (_ 10) (scroll-up 3) (redisplay t))
         (sit-for 0.3)
         (probe-out "再描画の後の子ビューの中心の色 => %S（赤のままなら Emacs は上書きしていない）"
                    (spike-probe (expand-file-name "shot-after-redisplay.png" probe-dir)))

         ;; P3: Lisp を占有している間に、別スレッドが何回 commit できるか
         (probe-out "\n* P3 Lisp が止まっている間の描画")
         (spike-commits)
         (let ((t0 (float-time)) (n 0))
           (while (< (- (float-time) t0) 1.4) (setq n (1+ n)))
           (probe-out "Lisp のビジーループ: %.2f 秒 / %d 回" (- (float-time) t0) n))
         (probe-out "%s" (spike-commit-log))

         ;; P4 / P5: マウスの経路と child frame
         (probe-out "* P4 / P5 マウスと child frame")
         (make-frame '((name . "second-frame") (width . 20) (height . 5)))
         (make-frame `((parent-frame . ,(selected-frame)) (name . "childframe-test")
                       (width . 20) (height . 5) (left . 100) (top . 100)
                       (minibuffer . nil) (undecorated . t)))
         (redisplay t) (sit-for 0.5)
         (probe-out "%s" (spike-hittest))

         ;; P6: フレーム → NSWindow の対応付け
         (probe-out "\n* P6 フレームと NSWindow の対応")
         (probe-out "window-id: %S"
                    (mapcar (lambda (f) (cons (frame-parameter f 'name) (frame-parameter f 'window-id)))
                            (frame-list)))
         (probe-out "ns-frame-list-z-order: %S"
                    (mapcar (lambda (f) (frame-parameter f 'name)) (ns-frame-list-z-order)))
         (probe-out "%s" (ivar-dump))

         (probe-out "\n* 子ビューの最終状態")
         (probe-out "probe => %S" (spike-probe (expand-file-name "shot-final.png" probe-dir))))
     (error (probe-out "ERROR: %S" err)))
   (kill-emacs 0)))
