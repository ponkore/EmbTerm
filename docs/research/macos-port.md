# macOS への移植の検討（Emacs NS 版への埋め込み）

- 日付: 2026-11-13
- **TODO T-121 の記録**（やるときの段階は TODO.md §9 の T-122〜T-128）
- 調べ方: 今のコード（`main` の `aa2974e`）の Windows 依存の洗い出しと、**手元の Mac で埋め込みの可否を実測した**
  （プローブは [macos-port/](macos-port/)。`./run.sh` で再現できる）
- 実測の環境
  - macOS（Darwin 27.2.0、aarch64）
  - **Emacs 31.1（emacsformacosx の NS 版**。`aarch64-apple-darwin23.6.0`、2026-08-24 ビルド、
    `system-configuration-features` に `MODULES`・`NS`、`module-file-suffix` = `".dylib"`）
  - ディスプレイは論理 2560x1440、`backingScaleFactor` = 2
  - Rust は入っている（`~/.cargo/bin`）。**zig は入っていない**。`vendor/` も無い（Windows で作ったもの）

**凡例**: 「実測」は 2026-11-13 に上のプローブで確かめたこと。「推測」「未確認」はそのとおり。
行数は `wc -l`（`main` の `aa2974e`）。

---

## 0. 要点

| 問い | 答え |
|---|---|
| **Emacs の NS フレームに自前のビューを重ねられるか** | **できる**（実測）。`EmacsWindow` → `contentView` → `EmacsView` の中に `addSubview:` した子ビューは、Emacs の再描画（200 行の挿入・`scroll-up` 10 回・`redisplay`）の後も消えない |
| **描画を Lisp スレッドから切り離せるか（レスポンスの根拠）** | **できる**（実測）。**Lisp をビジーループで 1.4 秒占有している間に、別スレッドから `CATransaction` の commit が 212 回通った**（1 回 0.02〜0.06 ms）。表示は Emacs の redisplay とも Lisp とも無関係に進む |
| 作業量 | **Rust の新規・書き直しが 4,000〜4,600 行**（既存 14,900 行のうち**約 6 割は再利用**）＋ `embterm.el` の差分 200 行ほど。最大の仕事は描画（Direct2D / DirectWrite → CoreText + Metal）で 1,500〜2,000 行（§4） |
| 全体の難しさ | **Windows 版より易しい部分が多い**。ConPTY 由来の難物（再配布版の同梱、幅の三者合わせ、win32-input-mode、Job Object）と tr-ime 依存、`HWND_BOTTOM` の工夫が**丸ごと消える**（§6） |
| 価値 | **ある**（§7）。ただし「mac の ghostel の遅さの出どころ」を先に測ってから着手するのが安全（T-122） |
| 止まりうる所 | 技術的に行き止まりになる点は見つからなかった。残る不確かさは**描画の書き直しの手間**と**IME の候補ウィンドウの位置**（§5-1、§5-2） |

## 1. Windows 版の前提のうち、macOS に無いもの

今の実装は「要件 §5.2 の埋め込み」と「§6.1 のスレッド」を Win32 の作法で満たしている。macOS では土台が違う。

| Windows 版の前提 | macOS では |
|---|---|
| 子 HWND を親フレームの HWND に `WS_CHILD` で付ける | 子 **NSView** を `EmacsView` に `addSubview:` する（実測で可。§3.1） |
| `(frame-parameter nil 'window-id)` が HWND | **`window-id` は NSWindow の番号ではない**（フレームごとの通し番号）。対応付けを自分で作る（§3.7） |
| 端末ごとに UI スレッド 1 本（HWND を持つスレッド）＋ `PostMessage` の列 | **ビューの操作は main スレッド（= Lisp のスレッド）だけ**。Lisp → module は直接呼び（同期）でよく、`PostMessage` の列は不要。描画だけを別スレッドに出す（§3.3） |
| 入力キューがフレームと連結し、UI スレッドが詰まると Emacs のキーが止まる | **この制約が無い**（AppKit のイベントは main スレッドで Emacs の都合で配られる） |
| Direct2D / DirectWrite + DXGI のフリップモデル | **CoreText + Metal（CAMetalLayer）**。`Present(1)` の代わりに `CAMetalLayer` の `nextDrawable` と `maximumDrawableCount` で待ちを作る |
| ConPTY（再配布版の `conpty.dll` + `OpenConsole.exe` を同梱） | **Unix の PTY**（`openpty` / `TIOCSWINSZ` / プロセスグループ） |
| IME は tr-ime で変換ウィンドウの位置を上書き、変換中の文字列は IME が描く | **変換中の文字列（working text）は Emacs がバッファに overlay で描く**ので子ビューの下に隠れる。ただし経路が Lisp に出ているので奪える（§5-2） |
| 子フレーム（corfu）より下に置くため `HWND_BOTTOM` | **不要**。child frame は別の NSWindow なので必ず上に出る（§3.5） |
| DPI 150% では DWM に引き伸ばされてぼやける（T-043） | **起きない**。論理 pt とデバイス px が分かれている（`backingScaleFactor`。§3.4） |

## 2. プローブ（実測の道具）

[macos-port/](macos-port/) に置いた。`./run.sh` で clang が module を 2 つ作り、`emacs -Q` を一瞬立ち上げて
`result.txt` と `shot-*.png` を書いて終わる（画面収録の許可は要らない。合成結果はプロセスの中で
`cacheDisplayInRect:toBitmapImageRep:` で読む）。

| ファイル | 中身 |
|---|---|
| `spike.m` | 子 NSView を作って重ねる（`spike-attach`）、合成結果の画素を読んで PNG に保存（`spike-probe`）、別スレッドから層を更新（`spike-bg-update` / `spike-commits`）、`hitTest` とウィンドウ一覧（`spike-hittest`） |
| `ivar.m` | `EmacsView` の ivar を Objective-C のランタイムで覗く（`ivar-dump`）。フレームとの対応付けの材料を探すため |
| `probe.el` | P1〜P6 を順に実行して `result.txt` に書く |
| `run.sh` | ビルドして `emacs -Q -l probe.el` を走らせる |

- 確かめた項目: P1 ビューの階層と埋め込み / P2 Emacs の再描画との共存 / P3 Lisp が止まっている間の描画 /
  P4 マウス（`hitTest`）/ P5 child frame との重なり / P6 フレーム → NSWindow の対応付け
- 注意: GUI の Emacs では `default-directory` が当てにならないので、`run.sh` は `PROBE_DIR` と絶対パスで渡す

## 3. 実測の結果

### 3.1 ビューの階層（P1）

```text
EmacsWindow（NSWindow。windowNumber は 2534 など）
└─ contentView : NSView        isFlipped=0、wantsLayer=0
   ├─ EmacsView                isFlipped=1、layer=EmacsLayer、frame={{0,0},{595,508}}
   ├─ EmacsScroller            （スクロールバーを出していれば）
   └─ ★ ここに子ビューを足せる（または EmacsView の中に足す）
```

- `contentView` の直下に足しても、`EmacsView` の**中**に足しても、どちらも成立した（実測）
- **`EmacsView` の中に足すほうがよい**（推測）: `EmacsView` は `isFlipped=1`（左上原点）で、
  `window-inside-pixel-edges` の座標系とそのまま一致する。ツールバー・タブバーの分の原点のずれも考えなくてよい

### 3.2 Emacs の再描画との共存（P2）

- 子ビューの層を赤で塗り、200 行挿入 → `scroll-up` を 3 行ずつ 10 回 → そのたびに `redisplay t` → 合成結果を読むと
  **中心の画素は `rgb(0.93,0.29,0.26)` のまま**（実測）。Emacs の描画は子ビューを上書きしない
- `EmacsView` は layer-backed（`EmacsLayer`）だが、子ビューは AppKit / CoreAnimation が上に合成する
- 前例: Emacs の NS 版は xwidget（WebKit）を同じやり方で `EmacsView` に載せている（`src/nsxwidget.m`。このビルドでは
  xwidget は無効だが、方式としての前例）

### 3.3 Lisp が止まっている間の描画（P3）— **レスポンスの根拠**

別スレッド（`dispatch_async`、`QOS_CLASS_USER_INTERACTIVE`）で `CATransaction` を commit し続けながら、
Lisp 側で `(while (< (- (float-time) t0) 1.4) ...)` のビジーループを回した（イベントループも回らない状態）。

| 項目 | 実測 |
|---|---|
| Lisp の占有 | 1.40 秒 / 403 万回のループ |
| 別スレッドの commit | **1.2 秒で 212 回**（約 177 回/秒。`usleep(5000)` で足踏みしているので上限ではない） |
| commit 1 回の時間 | **0.02〜0.06 ms** |
| commit を行ったスレッド | main ではない（`isMainThread = 0`） |

- つまり **PTY の読み → VT の解釈 → 描画を Lisp の外に出せば、Emacs が Lisp で詰まっていても画面は動く**。
  ghostel（端末の内容を Emacs のバッファのテキストとして描く）との構造的な差はここ
- CAMetalLayer への描画そのものを別スレッドで行うのは一般的な作法（Ghostty 本体も macOS で描画スレッドを持つ）。
  **ただし本番の形（CAMetalLayer + `nextDrawable` + `present`）での実測はしていない**（未確認。段階 1 で測る）
- ビューの**ジオメトリ**（`setFrame:` / `drawableSize`）は main スレッドで触る。描画スレッドとはロックで仕切る（推測）

### 3.4 座標系・Retina（P1）

- `EmacsView` は `isFlipped=1` で左上原点。`window-inside-pixel-edges` = `(10 2 570 478)` がそのまま使える
- `backingScaleFactor` = 2。Emacs の「画素」は**論理 pt**（`frame-pixel-width` = 595 に対し、合成結果のビットマップは
  1190x1016）。セルの寸法は pt で決め、`CAMetalLayer` の `drawableSize` と `contentsScale` にスケールを掛ける
- Windows の「DPI 150% で DWM に引き伸ばされてぼやける」問題（T-043）は起きない

### 3.5 child frame（corfu / posframe）との重なり（P5）

- Emacs の child frame は**別の NSWindow**（`parentWindow` が親フレーム。実測で `#2536 childframe-test parent=second-frame`）。
  ウィンドウは必ずビューより上に合成されるので、**corfu などが端末に隠れることは無い**
- Windows 版の「子 HWND を作ったら `HWND_BOTTOM`、動かすときは `SWP_NOZORDER`」（要件 §5.2）に当たる工夫が不要になる

### 3.6 マウス（P4）

- 子ビューを足した状態で `hitTest:` を呼ぶと**子ビューが返る**（実測）。つまり既定では**子ビューがマウスを受ける**
- 選べる形は 2 つ。**自前で受ける（案 X）を勧める**（推測）
  - **案 X: 子ビューで受ける（Windows 版と同じ）**。`acceptsFirstResponder` を NO にすればキーボードフォーカスは奪わない
    （`WS_EX_NOACTIVATE` + `MA_NOACTIVATE` に当たる）。クリックは `Event::Clicked` にして Lisp のポーリングで
    `select-window`。トラックパッドの慣性スクロール（`NSEvent` の精密デルタ）を自分で扱えるのも利点
  - 案 Y: `hitTest:` で nil を返して Emacs に素通しし、`track-mouse` で Lisp から座標をもらう。Lisp を経由するので
    ドラッグの追従が鈍る見込み

### 3.7 フレーム → NSWindow の対応付け（P6）— **自分で作る必要がある**

| 材料 | 実測 | 使えるか |
|---|---|---|
| `(frame-parameter f 'window-id)` | `"1"` `"2"` `"3"`（フレームを作った順の通し番号） | **NSWindow 番号ではない**（実際の `windowNumber` は 2534〜2536）。キャッシュのキーには使える |
| `EmacsView` の ivar | `emacsframe`（`struct frame *` = `0x76cd31e6b0` など）を持つ | module API から `struct frame *` を取る手が無いので直接は結べない |
| `[NSApp windows]` | **作った順**（z 順ではない） | タイトルや位置で引くのに使う |
| `ns-frame-list-z-order` | 前面から `("childframe-test" "second-frame" "*scratch*")` | `[NSApp orderedWindows]`（z 順）と突き合わせられる（未確認） |

**勧める形（推測）**: 端末を作るとき（または対応が未知のとき）だけ、そのフレームのタイトルを一時的に一意な文字列にし
（`title` フレームパラメータ）、`[NSApp windows]` からそのタイトルの `EmacsWindow` を引き、`window-id` をキーに
`EmacsView` を覚える。タイトルは即座に戻す。フレームのタイトルは既定ではバッファ名で、別のフレームと衝突しうるので、
一意な印を付けて引くのが確実。代わりに `ns-frame-list-z-order` と `orderedWindows` の順序 + 位置の照合でも引ける。

### 3.8 その他（部材の確認）

| 項目 | 実測 |
|---|---|
| libghostty-vt | リリースに **`ghostty-vt-1.0.0.xcframework.zip`** がある（`SHA256SUMS` を取得して確認）。Apple 向けのビルドが上流から出ているので、**zig を入れずに済む可能性がある**（未確認。段階 0 で確かめる） |
| Emacs の module 対応 | `module-file-suffix` = `".dylib"`、`(fboundp 'module-load)` = t。プローブの module が実際に読み込めた。なお `(featurep 'dynamic-modules)` は nil を返す（判定に使わない） |
| フォント | **`HackGen Console NF` がこの Mac にも入っている**（`~/Library/Fonts`）。Windows と同じ見た目にできる |
| 画像の共有メモリ（T-093 の `t=s`） | libghostty-vt が Windows では未対応で見送った経路。**macOS では使える見込み**（推測） |

## 4. 構成要素ごとの移植の見積もり

| クレート / ファイル | 現在の行数 | 作業 | 新規・書き直しの目安 |
|---|---|---|---|
| `crates/vt`（libghostty-vt の FFI） | 4,325 | xcframework か `zig -Dtarget=aarch64-macos` でリンクする（`build.rs`・`xtask`）。`png.rs`（WIC）を ImageIO か `png` クレートに差し替え | **~150**（95% 再利用） |
| `crates/pty`（ConPTY） | 1,138 | **Unix の PTY に書き直し**。`openpty` + `TIOCSWINSZ` + プロセスグループ + `SIGHUP`/`SIGKILL` + `waitpid`。再配布版の探索・Job Object・コマンドラインの引用・環境ブロックは不要になる | **~350**（単純になる） |
| `crates/render`（D2D / DWrite / DXGI） | 2,237 | **CoreText + Metal に書き直し**。グリフのアトラス方式（Ghostty / WezTerm / Alacritty と同じ）。`frame.rs`（セルの写し）・eaw の overlap / shrink の規則・画像の層の扱いは移植できる | **~1,500〜2,000**（+ MSL 100 行） |
| `crates/session` | 1,614 | `keys.rs` から win32-input-mode と `VkKeyScan` 系を外し、macOS のキー（Option = Meta、Command の扱い）を足す。クリップボードは NSPasteboard か Lisp 経由 | **~400**（80% 再利用） |
| `apps/embterm-module` | 3,831 | ペイン・選択・スクロール・イベント・Lisp の API はそのまま。Win32 のメッセージループと「端末ごとの UI スレッド」を「main スレッド + 描画スレッド」に置き換え。通知は `UNUserNotification` か `osascript` | **~1,000〜1,200** |
| `apps/embterm`（デバッグ用 exe） | 1,147 | NSWindow 1 枚のアプリにする。自動確認の引数（`--input` `--screenshot` `--exit-after`）は維持 | **~500** |
| `lisp/embterm.el` | 1,705 | tr-ime の節（§5-2）を ns 用に差し替え、配置・マウス・DPI の細部 | **~200**（90% 再利用） |
| `xtask` | 617 | `fetch-ghostty-vt` の macOS 版、`fetch-conpty` は Windows だけ、`dist` の中身 | ~100 |
| 合計 | 14,900 + el 1,705 | | **Rust 4,000〜4,600 + elisp 200** |

- 分け方は `#[cfg]` とトレイト（`Pty` / `Renderer` / ビューの置き場）で、**リポジトリは分けない**（推測。Windows が常用なので
  二重管理は避ける）。Windows 側を壊さない保険は既存の回帰試験（`cargo xtask test-screens` 38 + 3 画面、`test-emacs` 11 本）

## 5. macOS で新しく出る課題

1. **描画の書き直し（最大の仕事）** — Windows で「グリフのアトラスは要らない」と決めた（T-008 / T-077）のは
   Direct2D の `DrawGlyphRun` が速かったからで、macOS に同じ近道は無い。**CoreText でグリフをアトラスに焼き、Metal で
   インスタンス描画**するのが本筋。背景は「セル 1 つを 1 画素のビットマップ」と同じ考えでインスタンスの矩形にできる。
   `Present(1)` で止まっていた問題（T-068）に当たるのは `CAMetalLayer` の drawable の待ちで、**描画スレッドで待てば
   main スレッド（= Emacs）は止まらない**（推測）
2. **IME** — NS 版は変換中の文字列（working text）を **Emacs がバッファに overlay で描く**ので、そのままでは子ビューの
   下に隠れる。ただし経路が Lisp に出ている（`ns-put-working-text` → `ns-insert-working-text` / `ns-echo-working-text`、
   変数 `ns-working-text`。`term/ns-win.el`）ので、**端末のバッファではこれを奪って module に渡し、端末のカーソル位置に
   自前で描ける**（tr-ime より素直）。残るのは**候補ウィンドウの位置**で、これは
   `-[EmacsView firstRectForCharacterRange:actualRange:]` が決める。手は 2 つ（どちらも未確認）
   - ポイントを端末のカーソルに近い位置へ動かして、Emacs が返す矩形を寄せる
   - module から `firstRectForCharacterRange:` を method swizzling で差し替える（tr-ime の位置の上書きに相当）
3. **フレーム → NSWindow の対応付け** — §3.7。解決策はあるが `window-id` → HWND の一発では済まない
4. **マウスの経路の選択** — §3.6。案 X を勧める
5. **Retina・複数ディスプレイ** — セルをデバイス画素に合わせる。スケールの違うディスプレイへ移したときに
   `contentsScale` と `drawableSize` を作り直す（未確認）
6. **Emacs のビルドの差** — NS 版（emacsformacosx / emacs-plus）と Mitsuharu の emacs-mac ではビューの階層が違う。
   **NS 版 1 本に絞る**のが現実的（いま使っているのは emacsformacosx の 31.1）
7. **自動確認の移植** — `test-screens` はそのまま使える。`test-emacs` は Windows では `PostMessage` でマウスを送っていた
   （T-081）。macOS では合成イベントに権限が要るので、module に試験用の注入口を作る（推測）
8. **配布** — `.dylib` 1 枚 + `embterm.el`（`conpty.dll` / `OpenConsole.exe` が無くなる）。自分でビルドして自分の Emacs に
   読ませるだけなので署名・公証は要らない見込み（推測）。`cargo xtask dist` の中身を OS で分ける

## 6. macOS で消える課題

Windows 版の苦労のうち、次はそのまま不要になる。

| 消えるもの | 対応する TODO |
|---|---|
| 再配布版 ConPTY の同梱・検証・OS 同梱版との分岐 | T-007 / T-021 / T-072 |
| **幅の三者合わせ**（conhost が画面を組み直す問題）。端末とアプリの 2 者で済む | T-011 / T-025 |
| **win32-input-mode**（`CSI Vk;Sc;Uc;Kd;Cs;Rc _` の自前のエンコーダ、`VkKeyScanW` でのキーの作り直し） | T-022 / T-069 |
| **Job Object と breakaway するプロセス**（プロセスグループと `SIGHUP` で足りる） | T-041 / T-073 / T-076 |
| **tr-ime 依存**（変換ウィンドウの位置、再変換・前後参照のフック） | T-010 / T-083 |
| **`HWND_BOTTOM` と子フレームの前後関係** | 要件 §5.2 / T-044 |
| **入力キューの連結**（UI スレッドが詰まると Emacs のキーが止まる） | 要件 §6.1 |
| DPI 150% のぼやけ | T-043 |
| `WM_SETTINGCHANGE` が子ウィンドウに来ない等の Win32 固有の回り道 | genpreview-embedding.md §1.3 |

## 7. 価値の評価

**やる価値はある。** 根拠:

- 「レスポンスをもう少し」に**直接効く構造**であることを実測した（§3.3）。ghostel は端末の内容を Emacs のバッファの
  テキストとして描くので、出力のたびに Lisp と redisplay を通る。EmbTerm は読み・解釈・描画がすべて Lisp の外で、
  **Lisp が止まっている間もコンポジタに絵が届く**
- eaw の桁ずれ（`①` と claude）と全角グリフの縮みは `char-width-table` 由来なので、**mac の ghostel でも同じ制約**がある
  （未確認だが、ghostel の方式から見て同じになる見込み）。EmbTerm はここから自由
- **Windows と設定・キーマップ・`embterm.el` を共有できる**。2 つの OS を行き来する使い方では効果が大きい
- 下地が揃っている: 要件・TODO・回帰試験（vttest 38 画面 + Emacs 側 11 本）があり、移植の正しさを機械で確かめられる。
  libghostty-vt は Apple 向けのビルドが上流から出ている

**割り引いて見るべき点**:

- 描画バックエンドが 2 つになり、以後の機能追加が毎回 2 回ぶんになる
- mac の ghostel は「それなりに綺麗」なので、得られるのは主に**速さ**と**桁ずれの解消**で、見た目の改善幅は Windows より
  小さい見込み
- **着手の前に、mac の ghostel の遅さの出どころを測るのが安全**（T-122）。遅さが redisplay や Lisp スレッドの詰まりなら
  EmbTerm の効果は確実。もし入力の往復や PTY の読みの間隔が主因なら、得られる差は小さい

## 8. 進め方（段階 0〜4）

TODO.md §9 に T-124〜T-128 として起こした。

| 段階 | 内容 | 目安 | TODO |
|---|---|---|---|
| 判断 | mac の ghostel の遅さの出どころを測る | 1 時間 | T-122 |
| 設計 | `cfg` とトレイトでの platform 分岐（Windows を壊さない） | 半日 | T-123 |
| 0 | `et-vt` を macOS でリンク（xcframework か zig）＋ `et-pty` の Unix 版。VT だけのヘッドレスで `test-screens` を通す | 半日 | T-124 |
| 1 | CoreText + Metal のレンダラと `embterm`（NSWindow 1 枚）。画面比較の回帰試験で正しさを担保し、描画の負荷を測る | 1〜2 日 | T-125 |
| 2 | module（NSView の埋め込み・配置・キー・ポーリング・イベント）。ここで「Emacs の中で動く端末」が完成 | 1〜2 日 | T-126 |
| 3 | IME・マウス・選択 / コピー・Kitty graphics・OSC・通知・シェル統合 | 1〜2 日 | T-127 |
| 4 | 測定（mac の ghostel と比べる）と `test-emacs` の移植 | 半日 | T-128 |

**段階 0 から始める**のがよい（推測）。失敗するなら一番早く分かる場所で、半日で「心臓部は macOS で動く」ところまで
確かめられる。段階 0 を通したら requirements.md §3（「macOS / Linux は対象外【案】」）と §8 を書き換える。

## 9. 未確認・推測のまま残ること

| # | 内容 | どこで分かるか |
|---|---|---|
| 1 | `CAMetalLayer` に別スレッドから描いて present したときの実際のコマ落ち・遅れ（今回測ったのは `CATransaction` の commit だけ） | 段階 1 |
| 2 | xcframework を Rust からリンクできるか（zig を入れずに済むか） | 段階 0 |
| 3 | IME の候補ウィンドウの位置を寄せられるか（ポイントの移動 / swizzling） | 段階 3 |
| 4 | スケールの違うディスプレイをまたいだときの作り直し | 段階 1〜2 |
| 5 | `ns-frame-list-z-order` と `[NSApp orderedWindows]` の順序が一致するか | 段階 2 |
| 6 | mac の ghostel が実際に何で遅いのか | T-122 |
| 7 | Linux（pgtk / X11）への移植。PTY と描画の抽象は再利用できるが、埋め込みの作法は別物 | 今回は見ていない |

## 10. 参考

| | |
|---|---|
| [macos-port/](macos-port/) | 今回のプローブ（`run.sh` で再現） |
| [genpreview-embedding.md](genpreview-embedding.md) | Windows 側の埋め込みの設計と、そこで踏んだ穴 |
| [conpty.md](conpty.md) | §6 で「消える」と書いたものの出どころ |
| [../notes/t054-performance.md](../notes/t054-performance.md) | Windows での性能の実測（比較の基準） |
| `src/nsxwidget.m`（Emacs のソース） | NS 版が `EmacsView` に NSView を載せている前例 |
| `lisp/term/ns-win.el`（Emacs 付属） | `ns-put-working-text` / `ns-insert-working-text` / `ns-working-text`（IME の経路） |
