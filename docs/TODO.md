# EmbTerm TODO（課題・検討事項）

要件は [requirements.md](requirements.md)。ここは「やること・決めること・調べること」の一覧。

## 書き方

| 印 | 意味 |
|---|---|
| ⬜ | 未着手 |
| 🔄 | 作業中 |
| ✅ | 完了 |
| ⏸ | 保留（理由を書く） |
| ❌ | やらない（理由を書く） |

- 終わったら印を ✅ にして、行末に **日付と結論を 1 行**、詳しい記録があればその場所を書く
  - 例: `- ✅ **T-020** libghostty-vt を Windows でビルドする（2026-11-12）→ 静的ライブラリで通った。docs/spikes/libghostty-build.md`
- 番号は通し番号。**付け直さない**（requirements.md や記録から参照するため）
- 新しい課題は、合う節の末尾に次の番号で足す。どこにも合わなければ「§8 発生した課題」に足す
- 決定したことが requirements.md の【案】に関わるなら、requirements.md も直す

---

## 0. 進め方

- ✅ **T-001** project-memo.txt を requirements.md / TODO.md に整理する（2026-11-11）
- ✅ **T-002** requirements.md の【案】を見直す（要らないもの・足りないもの・優先度）（2026-11-11）→ 【案】はすべて了承。split と複数フレームの扱いを足した（T-016）
- ✅ **T-003** リポジトリを用意する（`git init`、Cargo の workspace、`.gitignore`）（2026-11-11）→ ブランチ `main`。設定は GenPreview にそろえた（edition 2024、CRT の静的リンク、release は LTO・`panic = "unwind"` のまま）。members はクレートを作るたびに足す（中身の無いディレクトリを glob で指すと Cargo がエラーにする）
- ✅ **T-004** 名前を決める（DLL・elisp の接頭辞・exe・クレート）（2026-11-11）→ GenPreview の慣習にそろえた。下の表

  | もの | 名前 | GenPreview での対応 |
  |---|---|---|
  | module の DLL | `embterm_module.dll`（クレート `embterm-module`、`(require 'embterm-module)`） | `genpreview_module.dll` |
  | elisp | `embterm.el`、接頭辞 `embterm-`（内部は `embterm--`） | `genpreview.el` |
  | module の関数 | `embterm-module-*` | `genpreview-module-*` |
  | デバッグ用 exe | `embterm.exe` | `genpreview.exe` |
  | 内部のクレート | `et-*`（例: `et-vt`、`et-pty`、`et-render`、`et-window`） | `gp-*` |

## 1. 大きな決定（実装の前に決める）

- ✅ **T-005** VT の解釈に libghostty-vt を使えるか（2026-11-11）→ **使う。1.0 の C ABI を静的リンクする。** docs/research/libghostty-vt-build.md
  - Zig 0.17.1 でソースからビルドし、静的ライブラリを Rust（MSVC、CRT 静的）の exe にも Emacs module にもリンクできた（T-020）
  - 1.0 の C ABI で、必要なものが揃っている: PTY への書き戻し、ベル、タイトル、cwd（OSC 7）、
    クリップボード（OSC 52 の読み書き）、デスクトップ通知、進捗、OSC 133、未知のシーケンス、
    同期出力（render hold）、reflow、render state（dirty の行だけの反復、画像）、キー・マウスのエンコーダ、selection、search
  - ghostel が C API で OSC を拾えず自前で走査していたのは 1.0 より前の C API の話（docs/research/ghostel.md §2）
  - ライセンス: 本体は MIT。同梱される highway・simdutf・wuffs・uucode も MIT / Apache-2.0 / BSD 系。配布物に表記を入れる
  - **eaw の幅を切り替える設定は無い**（幅 1 で固定）→ T-012
- ✅ **T-006** スレッドの構成を決める（2026-11-11: **初版の構成で T-027 を作り、T-068 で測って確定する**）→ **初版の構成で確定**（T-054 で測定。Lisp はどの場面でも止まらず、ロックの取り合いもほぼ無い。docs/notes/t054-performance.md §3.4）
  - 初版: 端末ごとに **PTY の読みのスレッド**（読む → 端末をロックして `vt_write` → 描き直しを UI スレッドに知らせる。
    知らせは AtomicBool でまとめて `PostMessage` 1 回）、**書きのスレッド**（チャネルで受けて PTY に書く。
    端末の応答（`WRITE_PTY` のコールバック）もここへ積む。ロックを持ったまま PTY に書かない）、
    **UI スレッド**（HWND を持つ。ロック → `render_state_update` → ロックを外す → 描く）
  - 端末 1 つにつき: UI（子 HWND のメッセージ）・PTY の読み・VT の解釈・描画をどう分けるか
  - 子 HWND のスレッドは詰まらせない（入力キューが Emacs と連結する）
  - Lisp → module は `PostMessage` のみ、module → Lisp はポーリング（GenPreview ADR 0006）
  - 材料: GenPreview は「ビューア 1 枚 = UI スレッド 1 本、頼みごとの列 + `PostThreadMessage`、
    Mutex の共有状態を 100 ms ごとにポーリング」。ghostel は「端末ごとに読みのスレッド 1 本が
    ロックを取って VT の解釈まで行い、描画は Lisp スレッド」。EmbTerm では読み・解釈・描画を
    すべて Lisp スレッドの外に出す
  - `PostThreadMessage` はモーダルループ（リサイズのドラッグ中など）で取りこぼすおそれ → HWND 宛ての `PostMessage` にする（推測。GenPreview の調査より）
- ✅ **T-007** PTY の実装を決める（2026-11-11）→ **ConPTY を `windows` クレートで直接叩く。再配布版 1.25 以降
  （NuGet `Microsoft.Windows.Console.ConPTY` 1.25.260930003）を同梱して優先し、OS 同梱版は退避先にする。**
  OS 同梱版（26300）は画面を作り直す旧式で、Kitty graphics（APC）と Sixel を捨て、OSC 52 / 9 / 777 / 7 と同期出力の位置をずらす。
  再配布版 1.22 以降は全部をその場で素通しする。passthrough のフラグはもう無い（0x8 は書記素の幅の意味に変わった）。
  docs/research/conpty.md §2、§5。ghostel の同梱は 1.24.2605
  - ConPTY を `windows` クレートで直接叩く / `portable-pty` などを使う
  - 再配布版 `conpty.dll` + `OpenConsole.exe` を同梱するか（ghostel は同梱して優先的に使う）
  - 材料: ghostel の手順（overlapped の名前付きパイプ、子のハンドルで終了を知る、`ClosePseudoConsole` は
    別スレッドで呼んで EOF まで読む、Windows では `RESIZE_PULL_SCROLLBACK` を false）はそのまま移せる。
    足りないもの: **Job Object**（孫プロセスを残さない）と、時限付きの終了待ち
- ✅ **T-008** 描画の方式を決める（2026-11-11: **初版は Direct2D / DirectWrite + DXGI のフリップモデル**（GenPreview と同じ）。
  負荷を T-068 で測り、足りなければグリフのアトラスに移る）→ **Direct2D / DirectWrite で確定。アトラスには移らない。** 文字を色ごとにまとめ（128 グリフずつ）、背景をビットマップ 1 枚にして、最悪の画面（314x70、セルごとに色が違う）で 80 → 6〜8 ms（t054 §3.3）
  - Direct2D / DirectWrite（GenPreview と共通）か、Direct3D + グリフのアトラスか
  - GenPreview で子ウィンドウが真っ黒になった件（DXGI のフリップモデル）の結果を確かめてから
  - 1 秒に何十回も描き直す前提での負荷
- ✅ **T-009** キー入力の経路を決める（2026-11-11）→ **案 A（フォーカスは Emacs に残し、キーマップで受けて送る）に確定。**
  ユーザーが手元の Emacs で確認: IME に違和感なし、claude で日本語入力と `Shift+Enter` が動く。`Ctrl+Space` は未確認（修飾キーの区別は T-069）
  - A: フォーカスを Emacs に残し、Emacs のキーマップで受けて module に送る（GenPreview・ghostel の形）
  - B: 子 HWND がフォーカスを取り、`WM_KEYDOWN` / `WM_CHAR` を自分で処理する。Emacs に戻すキーをどうするか
  - Emacs に残すキー（`C-x`、`M-x`、ウィンドウ移動）と端末に送るキーの分け方。
    eat の semi-char / char / emacs モードのような切り替えが要るか
  - `Esc`・`C-[`・`M-` の区別、`C-@`、ファンクションキー、修飾キーの組み合わせ
  - 材料: GenPreview と ghostel はどちらも A。ghostel の semi-char / char / copy の入力モードと、
    Emacs に残すキーの既定（`C-c` `C-x` `C-u` `C-h` `M-x` `M-:` `C-\`）は流用できる。
    ghostel は押したときしか送らないので、Kitty keyboard の「離したとき」の報告は満たせない
  - cua-mode に負けないよう、キーマップは `emulation-mode-map-alists` の先頭に置く（GenPreview f8a509f）
  - **ConPTY は win32-input-mode（`CSI ?9001h`）を必ず求める**（T-022）。cmd・PSReadLine・ネイティブ vim の
    修飾キーの区別（Ctrl+Space、Shift+Enter など）には、仮想キー・スキャンコード・文字・修飾キーの状態を送る必要がある
    - B なら `WM_KEYDOWN` からそのまま揃う
    - A では Emacs のイベントから `VkKeyScanW` / `MapVirtualKeyW` で作り直す。キーを離したイベント・AltGr・テンキーの区別は失われるが、
      「押した + 離した」を合成すれば実用上は足りる見込み（推測）
  - 送り方の切り替えは Windows Terminal と同じにする: アプリが KKP を求めた → libghostty-vt の KKP エンコーダ、
    そうでなく ConPTY が 9001 を求めている → win32-input-mode（自前）、それ以外 → レガシーの VT（T-069）
- ✅ **T-010** IME（日本語入力）をどう扱うか（2026-11-11）→ **案 A + tr-ime の位置の上書きで成り立った。** ユーザーが手元の Emacs で確認: 変換ウィンドウに違和感なし、文字の確定と claude のプロンプトでの日本語入力も OK。
  残る注意点（tr-ime の再変換が隠れたバッファに効く件）は T-083
  - A のとき: Emacs（tr-ime / w32-ime）が受けた確定文字列を送る。変換中の文字列は Emacs が
    バッファに描くが、**子 HWND の下に隠れて見えないおそれ**。候補ウィンドウの位置も端末のカーソルに合わせる必要がある
  - B のとき: 子 HWND で IMM32 / TSF を自分で扱う
  - claude のプロンプトで日本語を打つのが主な用途
  - 材料（T-024）: **A の心配は外れる見込み。** W32-IME では変換中の文字列を IME 自身のウィンドウが描く
    （隠れるのは quail などの Lisp の input method だけ）。位置は tr-ime の
    `tr-ime-modadv--set-composition-window` で画素指定できる。B は IME の状態が別の HIMC になり重い
  - 注意: tr-ime の再変換が隠れた端末バッファで `delete-char` を実行する。isearch を抜けると位置の上書きが外れる
    → 端末のバッファではフックをバッファローカルに差し替える
  - まず実測するもの: docs/research/emacs-ime.md §7 の P1（変換ウィンドウが子 HWND の上に出るか）、
    P3（tr-ime の上書き）、P5（確定文字の届き方）、P6（claude が入力位置に本物のカーソルを置くか）
  - 実装の材料（T-028）: 確定した文字は `self-insert-command` の remap で 1 文字ずつ届く見込み。カーソルの矩形は `embterm-module-cursor` で取れる。
    これを tr-ime の `tr-ime-modadv--set-composition-window` に渡せば変換ウィンドウを端末のカーソルに合わせられる
  - 2026-11-11: **位置合わせを embterm.el に実装した**（`embterm-ime-position`、既定 t。ポーリングのたびに、選んでいるウィンドウが端末なら
    カーソルの位置を STYLE 1 で渡し、離れたら STYLE 0 で戻す。isearch の後は入れ直す）。tr-ime の無い `emacs -Q` では何もしない。**手で打っての実測は未**
- ✅ **T-011** 文字幅を ConPTY（conhost）・VT ライブラリ・アプリの 3 者で揃える方法（2026-11-11）→ **再配布版の ConPTY を使えば揃う。**
  再配布版は既定で書記素クラスタ単位・eaw 1 桁で数え、libghostty-vt（mode 2027）・claude と一致する。
  OS 同梱の conhost は結合文字・ZWJ の列をまとめないのでずれる（docs/research/conpty.md §2.6）。アプリでの実測は T-025
  - ConPTY は自分の幅の判断で画面を組み直して送ってくる。ずれると桁が崩れる
  - ghostel（同じ ConPTY + libghostty-vt）では桁ずれが起きなかった実測がある（ghostel.md）
  - ghostel は mode 2027（書記素クラスタ）を既定で有効にしている。conhost は 2027 を知らないので、
    絵文字の ZWJ 列などで桁が食い違うおそれがある（推測）→ T-025 で測る
- ✅ **T-012** eaw の既定と設定を決める（2026-11-11）→ **幅 1 で固定。** 幅 2 にする設定は持たない（requirements.md §5.5）
  - **libghostty-vt の幅表は固定で、eaw は幅 1**（T-020 で実測。`①` `★` `※` `─` `○` `…` がすべて 1）。
    幅 2 を選べるようにするには libghostty-vt に手を入れる必要がある。ConPTY 側も幅 1 で数える前提
  - 案: 要件を「eaw は幅 1 で固定」に変える（claude・conhost・libghostty-vt の 3 者が揃う）
  - ConPTY 側には 1.25 から eaw を 2 桁にするフラグ 0x20 がある。ただしそれでも conhost は罫線 `─` を 1 桁のままにするので、
    libghostty-vt にも同じ例外を入れる必要がある
- ✅ **T-013** 1 桁と数えたのにフォントのグリフが全角の文字（`①` `★` `※` など）の描き方（2026-11-11）→ **2 つの方式を持ち、切り替えられるようにした。**
  既定は **overlap**（縮めずにセルの左端から描き、右隣へはみ出させる。WezTerm と同じ見え方。ユーザーの希望）。
  **shrink** は 1 セルに縮める（ghostel の規則）。`et_render::NarrowWide`、embterm.exe では `--narrow-wide shrink|overlap`。
  elisp からの切り替えは T-028 / T-029 で module の設定として出す（→ `embterm-narrow-wide`）
  - 既知の見え方（不具合ではない）: overlap では広いグリフが右隣のセルまで描かれるので、`★─` のように同じ色の文字が続くと
    `─` が `★` の腕に重なって見分けられない（WezTerm も同じ）。気になるときは shrink にする
  - 縮める / はみ出させる / 切る / 半角のグリフを持つフォント（HackGen Console NF など）に逃がす
  - WezTerm + HackGen Console NF では `①` が半角で出る（`~/.emacs.d/docs/claude/emacs-claude-pty-proxy-study.md`）
  - 材料: ghostel はセル幅に合わせて幅・ascent・descent の比の小さいほうで縮める。前後が空白で孤立しているときだけ
    右隣の空白を借りて 2 桁で描く（`Renderer.zig:699-812`）。この規則は自前の描画でもそのまま使える
  - T-027 で ghostel の規則をそのまま入れた。**`①②③` が 1 セルに縮んで小さく、読みにくい**（HackGen Console NF、セル 8x18 px）。
    縦は縮めずに横だけ詰める、半角の `①` を持つフォントを優先して探す、などを試す
- ✅ **T-014** module → Lisp の通知の形を決める（2026-11-11）→ **初版は世代付きの共有の状態を Lisp のタイマーで 50 ms ごとにポーリング**（端末があるときだけ）。
  中身はタイトル・cwd・ベル・終了・クリック・桁数と行数・カーソルの矩形。クリックからウィンドウが選ばれるまで最大 50 ms 遅れる。`open_channel` は使っていない（docs/notes/t028-module.md）
  - ghostel は CRT を静的リンクしたまま `open_channel` に書く回避策を持つ（emacs.exe の import 表から
    `_dup` を持つ CRT を探し、その `_write` / `_close` を使う。`ConPtyProcess.zig:78-219`）。
    ポーリングの代わりになるか検討する。emacs クレートが生の `emacs_value` を出さない点（ADR 0006）が障害
- ✅ **T-015** GenPreview とのコードの共有の仕方（2026-11-11）→ **必要な薄い層をコピーして作り替える。** path 依存はしない
  - 材料: path 依存は `gp-core` ごと引き込むので避ける。**薄い層（約 1,000 行）をコピーして作り替える**のが勧め。
    出発点は GenPreview の `spikes/embed-surface/src/lib.rs`（435 行で自己完結）
- ✅ **T-016** 同じ端末のバッファを 2 つのウィンドウに出したときの扱い（2026-11-11）→ split には対応し、ウィンドウごとに別のスクロール位置で見られること。複数フレームはきれいに見えなくてよい。requirements.md §5.2。実装は T-063
- ✅ **T-017** 端末の内容を Emacs のテキストとして扱う方法（2026-11-11）→ **copy モード（`C-c C-k`）に入ったときに、スクロールバックごとバッファに写す。** ポイントは見ていた位置に合わせ、戻るとペインの位置も元どおり。
  80,000 行で Emacs の止まりは最大 164 ms。docs/notes/t063-split-copy.md
  - 子 HWND の下のバッファは見えないので、`C-s` / `M-w` / occur がそのままでは効かない
  - 例: 画面とスクロールバックのテキストをバッファに写しておく（GenPreview と同じ）/
    コピーモードに入ったときだけ写す / 端末の中に検索を持つ
  - T-029 の copy モード（`C-c C-k`）は**画面だけ**を写す。スクロールバックも写すかをここで一緒に決める
- ✅ **T-063** split の実装方式を決める（2026-11-11）→ **実装済み。候補 B（Emacs のウィンドウごとに HWND を 1 枚、端末の状態は共有）。** UI スレッドと GPU デバイスは端末につき 1 つ。
  `test/split.el` で確認（NG 0）、画面は target/t063/。docs/notes/t063-split-copy.md
  候補 A は分割の間のモードライン・境界線を覆い、隣り合っていない配置に対応できないので採らない。
  スクロール位置はペインごとに module で持ち、libghostty-vt のビューポート（端末に 1 つ）は描く直前にそのペインの位置へ動かして render state を写す。
  端末の大きさは選んでいるペインに合わせ、ほかのペインは自分の大きさで切り取って描く）
  - 候補 A: **1 つの HWND の中を複数のペインに分けて描く**（落としどころの候補）。HWND の範囲と
    ペインの位置を Emacs のウィンドウ構成からどう決めるか（間のモードライン・境界線を覆わないか）
  - 候補 B: Emacs のウィンドウごとに HWND を 1 枚ずつ作り、同じ端末の状態を共有して描く
  - **端末の大きさ（桁数・行数）は ConPTY に 1 つしか伝えられない。** 大きさの違う 2 つのウィンドウに
    出したとき、どちらに合わせるか（選んでいるウィンドウ / 最後に選んだ / 小さいほう）と、合わないほうの描き方
  - ウィンドウごとのスクロール位置をどこで持つか（module のペイン / Emacs の window-parameter）
  - キー入力・カーソル・選択範囲は、どのペインのものとして扱うか
  - 複数フレームのとき: 最初のフレームにだけ出す、などの簡単な扱いでよい
  - 材料: GenPreview は同じバッファを 2 つのウィンドウに出してもビューアは 1 枚だけ（選んでいるウィンドウ →
    前に出していたウィンドウ → 最初のウィンドウの順）。複数フレームは `SetParent` で付け替える。
    候補 A は上下の split でモードラインを、左右の split で境界線を覆うおそれがある（推測）

## 2. 調査・スパイク

- ✅ **T-018** GenPreview の埋め込みの実装と、その後に分かったことを読む（2026-11-11）→ docs/research/genpreview-embedding.md。段階 A〜C まで実装済み。子ウィンドウでフリップモデルを使っても真っ黒にならない（50 回中 50 回正常）。split ではビューアは 1 枚だけで、ウィンドウごとのスクロール位置の前例は無い
- ✅ **T-019** ghostel の実装を読む（2026-11-11）→ docs/research/ghostel.md。今の ghostel は Zig の API を使う。ConPTY の手順はそのまま移せるが、Job Object が無く孫プロセスが残りうる。描画を Lisp スレッドがロックを持ったまま行う点は真似しない
- ✅ **T-020** libghostty-vt を Windows でビルドし、Rust から呼ぶ最小のスパイク（2026-11-11）→ 通った。docs/research/libghostty-vt-build.md、spikes/vt-link、spikes/vt-module
- ✅ **T-021** ConPTY がどのシーケンスを素通しするか調べる（2026-11-11）→ docs/research/conpty.md §2。OS 同梱版と再配布版 1.22 / 1.24 / 1.25 / 1.26 を実測した。
  再配布版はすべてをその場で素通しし、問い合わせへの端末の応答もアプリに届く（T-007）。
  1.25 以降は未知のシーケンスとリサイズの後に `CSI 6n` で位置を取り直す → 端末は正しくすぐに答える必要がある（T-070）。
  実測の道具は docs/research/conpty-probe/
- ✅ **T-022** キーの送り方を調べる（2026-11-11）→ docs/research/conpty.md §3。ConPTY は win32-input-mode と
  フォーカスの報告（`?1004h`）を必ず求める。コンソールのアプリに KKP の列を送ると、再配布版ではゴミ文字になる。マウスは ConPTY を通る。
  アプリごとの入力モード（claude・nyagos・git bash・yazi）は未実測 → T-045〜T-052 で確かめる
- ✅ **T-023** yazi の画像プレビューが Windows + ConPTY で出る経路を調べる（2026-11-11）→ docs/research/conpty.md §4。
  **再配布版の ConPTY なら Kitty graphics を通せる。** yazi の画像の問い合わせと `CSI 16t`（セルの画素の大きさ）に答える必要がある。
  ConPTY にいちばん合う Unicode プレースホルダ方式は、XTVERSION の応答に "ghostty" を含むときだけ yazi が選ぶ（T-071）。
  ghostel の Windows で画像が出た報告は見つからなかった
- ✅ **T-024** Emacs 31.1 の w32 の IME の扱いを読む（2026-11-11）→ docs/research/emacs-ime.md。変換中の文字列は IME 自身のウィンドウが描くので、子 HWND の下には隠れない見込み。位置は Emacs がポイントから決め、tr-ime の画素指定で上書きできる。確定した文字列は 1 文字ずつキーイベントで届く。**案 A + tr-ime で成り立つ見込み**。実測は P1〜P9（文書の §7）
- ✅ **T-025** claude の文字幅を確かめる（2026-11-11）→ EmbTerm の中の claude で、`①②③` を入れた罫線の表が崩れないことをユーザーが確認した。Windows Terminal などとの見比べと、ZWJ の絵文字は確かめていない
- ✅ **T-026** `emacs` クレートの版と Emacs 31.1 の module API の確認（2026-11-11）→ GenPreview と同じ `emacs` 0.23（feature `emacs-28`）で、Emacs 31.1 に読み込めた（spikes/vt-module）。`open_channel` を使うかどうかは T-014
- ✅ **T-121** macOS（Emacs の NS 版）へ移植できるかを調べる（2026-11-13）→ **移植できる。価値もある。** docs/research/macos-port.md、プローブは docs/research/macos-port/
  - 実測（`emacs -Q`、Emacs 31.1 の NS 版 / aarch64）: **`EmacsWindow` → `contentView` → `EmacsView` の中に子 NSView を足せ、Emacs の再描画（200 行の挿入・`scroll-up` 10 回・`redisplay`）でも消えない**
  - 実測: **Lisp をビジーループで 1.4 秒占有している間に、別スレッドから `CATransaction` の commit が 212 回通った**（1 回 0.02〜0.06 ms）。
    描画を Lisp から切り離せる = ghostel との差（レスポンス）の根拠
  - 実測: child frame（corfu）は別の NSWindow なので必ず端末より上に出る（`HWND_BOTTOM` の工夫が要らない）。`backingScaleFactor` = 2 で DPI のぼやけ（T-043）は起きない。`HackGen Console NF` は mac にも入っている
  - **`(frame-parameter nil 'window-id)` は NSWindow の番号ではない**（フレームごとの通し番号）。対応付けを自前で作る（macos-port.md §3.7）
  - 見積もり: Rust の新規・書き直し 4,000〜4,600 行（既存 14,900 行の約 6 割は再利用）＋ `embterm.el` 200 行。最大の仕事は描画（D2D / DirectWrite → CoreText + Metal）
  - ConPTY 一式・幅の三者合わせ（T-011）・win32-input-mode（T-069）・Job Object（T-073）・tr-ime（T-010 / T-083）は**丸ごと不要になる**

## 3. 最小構成（一本通す）

- ✅ **T-027** デバッグ用 exe: トップレベルのウィンドウ + ConPTY + VT + 描画で cmd が動く（2026-11-11）→ **cmd・pwsh・vim が動いた。**
  クレート `et-vt`（手書きの FFI、型の配置を `ghostty_type_json()` と照合するテスト）・`et-pty`（再配布版優先、Job Object）・
  `et-render`（D2D / DirectWrite、T-013 の縮め方）と `apps/embterm`。自動確認の引数（`--input` `--screenshot` `--exit-after` など）付き。
  docs/notes/t027-debug-exe.md
- ✅ **T-028** module: 同じものを Emacs のフレームの子 HWND として出し、pwsh が動く（2026-11-11）→ `apps/embterm-module`（`embterm_module.dll`）。
  端末ごとに UI スレッド 1 本。頼みごとは端末が生きている間ずっと残るメッセージ専用ウィンドウ宛ての `PostMessage`。
  別に起動した `emacs -Q` で cmd・pwsh・分割・リサイズ・バッファの切り替え・別フレームへの付け替え・フレームを消した後の作り直しを確かめた。
  30,000 行の出力の最中も Emacs の 10 ms タイマーの間隔は最大 20〜29 ms。docs/notes/t028-module.md、画面は target/t028/
- ✅ **T-029** `embterm.el`: 起動・位置合わせ（フック 2 つ）・キーの送信・終了（2026-11-11）→ `lisp/embterm.el`。`M-x embterm`、semi-char の入力（`embterm-keymap-exceptions`）、
  copy モード（`C-c C-k` で画面のテキストをバッファに写す / `C-c C-j` で戻る）、タイトル・cwd の反映、終了でバッファを消す、
  defcustom（`embterm-font-family` `embterm-font-size` `embterm-narrow-wide` `embterm-shell` `embterm-module-directory`）。
  子プロセスは最初にウィンドウに出したときに起動する。試験は test/smoke.el（`emacs -Q -l test/smoke.el`）
- ✅ **T-030** DLL の更新の手順（2026-11-11）→ GenPreview と同じく、更新時刻とサイズを名前に入れて一時ディレクトリにコピーしてから読む。
  conpty.dll と `x64/OpenConsole.exe` は元の場所のものを使う。新しい版を使うには Emacs の再起動が要る。配布物は `cargo xtask dist`（`target/dist/`）
  - GenPreview の方式: 更新時刻とサイズを名前に入れて一時ディレクトリにコピーしてから読む。新しい版を使うには Emacs の再起動が要る
- ✅ **T-031** module の境界で panic を止める（2026-11-11）→ wndproc・自前のスレッドの本体・user-ptr の `Drop` を `catch_unwind` で包んだ（`#[defun]` は emacs クレートが守る）
  - `#[defun]` は emacs クレートが Lisp のエラーに変える。**user-ptr の finalizer（`Drop`）と、自前のスレッドの本体と、
    wndproc は守られない**ので自分で `catch_unwind` する（GenPreview の調査より）

## 4. 機能

- ✅ **T-032** リサイズ（ウィンドウの大きさ → 桁数・行数 → ConPTY）と reflow（2026-11-12）→ 足りていた。pwsh で狭める・広げる・続けざまに 20 回変える・縦に縮めて戻す、のどれでも折り返し直され、中身が欠けず、二重に出ず、アプリから見た大きさも一致。縦に戻すと空きが下に残るのは `resize_pull_scrollback = false` の想定どおり。docs/notes/t032-features.md、`test/resize.el`
- ✅ **T-033** フォントの設定とフォールバック（日本語、Nerd Font のアイコン）（2026-11-12）→ `embterm-font-fallback-families`（主フォントに無い文字に Windows のフォールバックより先に使う。`IDWriteFontFallbackBuilder`）、`embterm-font-family` に候補のリスト、`text-scale-adjust` でバッファごとに拡大・縮小。合字はしない。docs/notes/t032-features.md、`test/font.el`
- ✅ **T-034** 色とテーマ（2026-11-11）→ `embterm-follow-theme`（既定 t）で Emacs のテーマ（`default`・`cursor`・`region`・`ansi-color-*`）に追従し、テーマの切り替えで全端末に反映。アプリが OSC で変えた色は尊重。docs/notes/t034-integration.md
- ✅ **T-035** カーソル（2026-11-11）→ `embterm-cursor-style`・`embterm-cursor-blink`。アプリの DECSCUSR を優先。点滅は選択中で見えていてフォーカスのあるペインだけ、カーソルのセルだけ描き直す
- ✅ **T-036** 選択・コピー（kill-ring にも入れる）・貼り付け（ブラケットペースト）（2026-11-11）→ ドラッグ・ダブル / トリプルクリック・Shift+ドラッグ（マウスの報告が有効なとき）、`embterm-copy-on-select`、`M-w`、`C-y`。実際のクリップボードへの到達は `gui-select-text` まで確認
- ✅ **T-037** スクロールバック（行数の設定、ホイール、キーでのスクロール）（2026-11-11）→ ペインごとのホイール、Emacs 側のホイールの転送、`S-<prior>` / `S-<next>` / `S-<home>` / `S-<end>`、入力で下へ戻る、`embterm-scrollback-lines`。libghostty のバイト上限の既定（10,000 バイト）では数百行しか残らないので無制限にした
- ✅ **T-038** マウスの報告（vim / yazi）と、Emacs 側の選択との切り替え（2026-11-11）→ クリック・ドラッグの報告（vim の `:set mouse=a` で位置を確認）、代替画面のホイールを矢印に変える（DEC 1007）、マウスの形。**マウスの形は目視していない**、yazi での確認は T-052
- ✅ **T-039** OSC（2026-11-11）→ OSC 52 の書き込みは kill-ring へ、**読み取りは既定で拒否**（`embterm-allow-clipboard-read`）。OSC 9 / 777 の通知は `embterm-notification-functions`、OSC 9;4 は `embterm-progress-functions`。OSC 8 は `Ctrl+クリック`で開く（`file:` は `find-file-other-window`、http / https / ftp / mailto / file 以外は開かない）。リンク上の手の形のマウスは手で確認が要る
- ✅ **T-040** ベル（2026-11-11）→ `embterm-bell-function`（既定 `ding`）。間引く（1 秒に 100 回で 5 回）
- ✅ **T-041** 後始末（プロセスの終了、バッファの削除、Emacs の終了。子プロセスまで止める）（2026-11-12）→ 足りていた（T-073 / T-076）。`exit` で OpenConsole.exe まで終わる、`embterm-kill-buffer-on-exit` nil でバッファが残る、**Emacs の異常終了でも子孫と OpenConsole.exe が残らない**を確かめた。docs/notes/t032-features.md、`test/cleanup.sh`
- ✅ **T-042** 複数の端末（2026-11-11）→ `embterm`（既存があれば切り替える）、`embterm-new`、`embterm-project`、`embterm-list-buffers`、`embterm-sync-colors`
- ✅ **T-043** DPI（2026-11-11）→ 150% のモニタで見え方に問題が無いことをユーザーが確認した。モニタをまたいだ移動は個別に確かめていない
- ✅ **T-044** Emacs 側の操作との組み合わせ（タブ、子フレーム（corfu）との重なり、最小化、フレームの削除）（2026-11-12）→ 足りていた。tab-bar（バーの分ずれる・別のタブで消える）、子フレームが端末の上に出る、最小化の間の出力、フレームの削除を確かめた。タブを移るとスクロール位置が失われる件は T-087。docs/notes/t032-features.md、`test/frames.el`

## 5. 目標のアプリで確かめる

requirements.md §4 の「とくに確かめること」を見ながら。

- ✅ **T-045** cmd（2026-11-11）→ 日本語・`dir`・eaw の表示 OK（入力は PTY へ直接送ったので、キーのエンコードは T-091 / test/keys.el の確認に任せる）
- ✅ **T-046** PowerShell 5.1（2026-11-11）→ 5.1.26100 で日本語の表示・Tab 補完・履歴・行内の移動 OK（入力は PTY へ直接送ったので、キーのエンコードは T-091 / test/keys.el の確認に任せる）
- ✅ **T-047** pwsh（PSReadLine、starship）（2026-11-11）→ 7.6.9 で日本語・Tab 補完 OK。キーはユーザーが手元で確認（T-091）。starship は nyagos のプロンプトで確認
- ✅ **T-048** nyagos（2026-11-11）→ 4.4.20 で Powerline のプロンプト・Nerd Font・履歴・色付き `ls`・eaw の表示 OK（`M-x shell` では使えなかったもの）（入力は PTY へ直接送ったので、キーのエンコードは T-091 / test/keys.el の確認に任せる）
- ✅ **T-049** wsl bash（2026-11-11）→ Ubuntu で `TERM=xterm-256color` / `COLORTERM=truecolor`、日本語、`Ctrl+C`（終了コード 130）、less の検索 OK（入力は PTY へ直接送ったので、キーのエンコードは T-091 / test/keys.el の確認に任せる）
- ✅ **T-050** git bash（2026-11-11）→ `--login -i`（UCRT64）で日本語、履歴と Backspace、`Ctrl+C`、less、true color・波線の下線・太字斜体 OK（入力は PTY へ直接送ったので、キーのエンコードは T-091 / test/keys.el の確認に任せる）
- 🔄 **T-051** claude（2026-11-11: 桁ずれ・日本語入力・`Shift+Enter`・`C-c C-c` はユーザーが確認。長い出力と通知が残り）
- ✅ **T-052** yazi（画像のプレビュー、マウス）（2026-11-11）→ embterm.exe と別起動の `emacs -Q` で、プレビュー・`j` での切り替え・`q` で画面が戻る・クリック・ホイールを確認。`Drivers.matches: Kgp`。手元での確認は T-091
- ✅ **T-053** vim（2026-11-11）→ 日本語入力の表示・`:set number`・`Esc`（T-069 の `:set mouse=a` のクリックも確認済み）OK。`-u NONE` の MSYS 版 vim は encoding が latin1 になり化けるが vim の設定の問題
- ✅ **T-054** 性能を測る（大量の出力、入力の遅れ、端末を何個も開いたとき）（2026-11-11）→ Emacs の 10 ms タイマーの p99 はどの場面でも 17〜22 ms（止まらない）。`cat` 10 MB は 5.3 秒で Windows Terminal と同じ（ConPTY が律速）。入力から `Present` まで 1〜2 ms。端末 15 個でプライベート 123 MB。docs/notes/t054-performance.md、`test/perf.el`
- ✅ **T-055** 端末としての適合を測る（vttest など）（2026-11-11）→ vttest 2.7 のメニュー 1〜11。ふだん使う範囲は合格。落ちたのは libghostty-vt が持たない機能（DECDWL/DECDHL、VT52 の画面、矩形の操作、いくつかの報告。Ghostty 本体と同じ）。ConPTY は出力をそのまま通す（ConPTY を通さない画面と 100 画面あまり一致）。DECCOLM の後に端末の大きさが窓と食い違ったまま残るのを直した。docs/notes/t055-vttest.md

## 6. ghostel からの移行

- ✅ **T-056** ghostel で使っている機能とキー割り当てを洗い出す（`C-c s s` / `n` / `p` / `b`、`my-ghostel.el`）（2026-11-11）→ 4 つのコマンド・pwsh の起動・module の置き場はすべて EmbTerm にある。足りなかった `setenv` の受け渡し（T-109）と `embterm-run`（T-110）を足した。docs/notes/t056-ghostel-migration.md
- 🔄 **T-057** `~/.emacs.d` に `my-embterm.el` を作り、ghostel と並べて使ってみる（2026-11-11: `my-embterm.el` と関連の変更をパッチにした（`~/.emacs.d` への書き込みは権限の判定で止まった）。`emacs -Q` で動作を確認済み。利用者がパッチを当て、~/.emacs.d の e2abb3e としてコミット・プッシュした。普段の Emacs で ghostel（C-c S）と並べて使う段階）
- ⏸ **T-058** ghostel をやめる判断と撤去（`my-ghostel.el`、`~/.emacs.d/ghostel/`、ドキュメント）（2026-11-11: 利用者は EmbTerm を常用すると決めた。ghostel の機能に EmbTerm へ移せて有用なものが無いかを利用者が調べる間、`C-c S` に残す）
- ✅ **T-059** `my-pty`（ptyd + eat）も一緒にやめられるか判断する（2026-11-11）→ やめた。依存していたのは init.el だけで、eat も my-pty だけが使っていた。`my-pty.el`・`ptyd/` を消し、ドキュメントは経緯として残した（~/.emacs.d の 5175707）。`M-x my:claude-term` も無くなった（`embterm-run` で作り直せる）
- ✅ **T-118** シェルから Emacs の関数を呼ぶ（ghostel の `ghostel_cmd` / `ghostel-eval-cmds` 相当。例: シェルで `ef foo.txt` → Emacs で `find-file-other-window`、`ed .` → `dired-other-window`）
  - 形式は **OSC 51;E**（vterm と同じ。`ESC ] 51;E "関数名" "引数" … ESC \`）。ghostel の OSC 52;e は libghostty-vt がクリップボードとして消費するので横取りできない
  - 受け口: libghostty-vt の `GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE` + `OPT_UNKNOWN_MAX_BYTES`（2048 までは追加の確保なし）で未知の OSC を受ける。再配布版 ConPTY（1.25）は未知の OSC もそのまま流す（後に conhost が `CSI 6n` で位置を取り直すのは対応済み）
  - 経路: `crates/vt` の `Effects` に `unknown_osc` → `crates/session` で `51;E` の、切れていない（`truncated` でない）ものだけ `Event` にする → module の `:events` に `(command . "…")` → `embterm.el` の `:events` の分岐で `split-string-and-unquote` して許可リストの関数を `condition-case` 付きで `apply`（`eval` はしない）
  - 許可リストの defcustom（既定は `find-file` / `find-file-other-window` / `dired` / `dired-other-window` / `message` 程度）と、機能全体を切る defcustom。端末に流れた文字（`cat` したファイルなど）でも発火するため。端末の窓が入れ替わらないよう `-other-window` 版を勧める
  - シェル側: pwsh の関数 `embterm_cmd`（引数を `"…"` で包み `\` と `"` をエスケープ、パスは `[IO.Path]::GetFullPath($p, $PWD.ProviderPath)` で絶対パスにしてから送る）。リポジトリに `etc/` として置くか、`~/.emacs.d/etc/pwsh-emacs-shell.ps1` に足すかを決める。nyagos（lua）も
  - （2026-11-12）→ 上のとおり実装。シェル側はリポジトリの `etc/embterm.{ps1,bash,lua}`（配布物の `etc/` に入る）。`embterm_cmd`・`embterm_find_file`・`embterm_dired`。pwsh・Git Bash・nyagos で確かめた（日本語と空白のパス、`"` と `\`、許可していない関数・切れたもの・機能を切ったときは呼ばない）。docs/notes/t118-shell-integration.md、`test/shellcmd.el`
  - 試験: `test/osc-child.py` に OSC 51;E を足し、許可したものだけ呼ばれる・許可していないものは呼ばない・切れたものは捨てる・非 ASCII のパス、を確かめる
- ✅ **T-119** OSC 133（プロンプトの境界）を使う（ghostel の semantic prompt 相当）: 前後のプロンプトへ飛ぶ、直前のコマンドの出力を kill-ring / copy モードへ写す、終了コードを出す、など
  - libghostty-vt は OSC 133 を解釈してセルに `semantic_content` を持つ。C ABI でセルの種別・行の印を読めるか、コールバックがあるかを先に確かめる
  - シェル側: pwsh のプロンプトで `133;A`（プロンプトの始め）/ `133;B`（入力の始め）/ `133;C`（出力の始め）/ `133;D;終了コード` を出す（starship と並べる。T-118 と同じ初期化スクリプトに）
  - キー割り当て（例: `C-c C-p` / `C-c C-n`）は Emacs 側のスクロールバックの表示（T-037）とどう組むかを決める
  - （2026-11-12）→ `C-c C-p` / `C-c C-n`（前後のプロンプトをペインの一番上に）、`C-c C-o`（直前の出力をコピー）、終了コードをモードラインに・`embterm-command-end-functions`。シェル側は `etc/embterm.{ps1,bash,lua}` に足した。pwsh・Git Bash・nyagos と、普段の starship の 2 行のプロンプトで確かめた。docs/notes/t118-shell-integration.md、`test/prompt.el`

## 7. ドキュメント（ある程度進んでから）

- ⬜ **T-060** 設計が固まり始めたら `docs/architecture.md` と `docs/adr/` を作る（GenPreview と同じ形）
- ⬜ **T-061** CLAUDE.md を作る
- ⬜ **T-062** README.md を作る

## 8. 発生した課題（随時追加）

- ✅ **T-064** libghostty-vt の取得とビルドを `cargo xtask` にする（2026-11-11）→ `cargo xtask fetch-ghostty-vt [--force]`。
  `vendor/ghostty-vt` に置く。`-Dtarget=x86_64-windows-msvc` を明示しても結果は同じ。リンカの警告は出ない
- ✅ **T-065** libghostty-vt の Rust の宣言を bindgen で作るか、使う範囲だけ手で書くか（2026-11-11）→ **使う範囲だけ手で書く。**
  構造体の大きさとフィールドの位置は、libghostty-vt が返す `ghostty_type_json()` とテストで照合する（libclang は入れない）
- ✅ **T-066** 端末の状態を HWND より長生きさせる（2026-11-11）→ session（端末 + PTY）と view（子 HWND）を分けた。フレームを消しても session は残り、次の置き場所の指示で作り直すか `SetParent` で付け替える。（フレームを消すと子 HWND とそのスレッドも消える。
  GenPreview は文書を読み直して作り直すが、端末は作り直せない）
- ✅ **T-067** GPU のデバイスを端末どうしで共有し、デバイスを失ったときは全員で作り直す（GenPreview はビューアごとに作っている）（2026-11-11）→ `Gpu::shared` + 世代の番号。失ったら全端末に `WM_APP_DEVICE`。15 個でデバイス 15 → 1、プライベート 438 → 123 MB。作り直しは擬似的な消失で確認（`test/close.el`）。本物の TDR は未確認
- ✅ **T-068** 描き直しの負荷を測る（GenPreview のスパイクは「1 回塗る」だけ。垂直同期を待つ `Present(1)` で UI スレッドが止まる量は未測定）（2026-11-11）→ `Present(1)` で 1 フレーム 16 ms 止まり、頼みごとが最大 50 ms 待っていた。**待てるスワップチェーン + `MsgWaitForMultipleObjectsEx`** にして `Present` 0.04 ms、頼みごとの待ち p99 1〜2 ms。描けない間は写さない
- ✅ **T-069**（2026-11-11）→ **KKP（アプリが求めたとき）→ win32-input-mode（自前）→ レガシーの順に切り替える。** `C-SPC`・`S-<return>`・`C-S-<英字>`・F1〜F24 の修飾・テンキーを区別して送る。文字は `Vk=0` + `Uc`。
  KKP のフラグが残ったままアプリが落ちた場合の安全策あり。受け取ったキーを記録する `test/key-child.py` + `test/keys.el` で確認（docs/notes/t069-input.md）。実機の IME を通した再確認は T-091。win32-input-mode のエンコーダを自前で作る（libghostty-vt には無い。`CSI Vk;Sc;Uc;Kd;Cs;Rc _`）。
  KKP / win32-input-mode / レガシーの切り替え。KKP のフラグが残ったままコンソールのアプリに戻ったときにゴミ文字を入れない安全策。
  IME の確定文字列は `Vk = 0` + `Uc` の形でよいか、サロゲートペア、長い貼り付けの速さ（docs/research/conpty.md §3.5）
- ✅ **T-070**（2026-11-11）→ `CSI 6n` は 2 ms 以内に応答、`CSI 16t` はセルの画素の大きさを返す、フォーカスは出入りとも届く。ConPTY が端末に求める応答をそろえる: `CSI 6n`（カーソル位置）にすぐ正しく答える、フォーカスの出入りで
  `CSI I` / `CSI O` を必ず送る、`CSI 16t`（セルの画素の大きさ）に答える
- ✅ **T-071**（2026-11-11）→ XTVERSION は `EmbTerm 0.1.0 (libghostty-vt 1.0.0)`、DA1 は `?62;22c`（28 と 4 を名乗らない）。`TERM_PROGRAM=EmbTerm` のまま。DA1 と XTVERSION で何を名乗るか決める。DA1 には libghostty-vt が本当に対応するものだけを出す
  （28 を出すと conhost が DECCRA / DECFRA を使い、4 を出すと yazi が Sixel を選ぶ）。
  XTVERSION に "ghostty" を含めると yazi が Kitty graphics の Unicode プレースホルダ方式を選ぶ（含めないと直接配置方式になる）
- ✅ **T-072** 再配布版 ConPTY の取得を `cargo xtask` にする（2026-11-11）→ `cargo xtask fetch-conpty`。NuGet 1.25.260930003 を
  SHA256 と Authenticode の署名（Microsoft）で確かめ、`vendor/conpty/conpty.dll` + `x64/OpenConsole.exe` に置く。
  embterm.exe は起動直後に `CSI c` が届くかで再配布版か OS 同梱版かを見分けてログに出す
- ✅ **T-073**（2026-11-11 → 原因は Microsoft Store の pwsh（MSIX）の子が既定でデスクトップアプリの環境の外に作られること。`PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY` で止めた。アプリの実行エイリアス経由で起こしたもの（cmd の中で `pwsh`）は Job に入らないので、閉じるときに親子関係で辿って止める。t054 §3.7）（T-069 の調べでも再現: pwsh の子は Job に入らないことがあり、プロセスの親子関係でしか追えない）Job から抜けるプロセス: 外側の Job が breakaway を許すとき、pwsh の `Start-Process` で起こした孫が Job から抜けた。
  原因と、抜けさせない方法を決める（T-041 の一部）
- ✅ **T-074**（2026-11-11）→ render hold の間は全ペインで写し直さない（時限 1 秒）。途中の画面が見えないことを確認。同期出力（DEC 2026）: `GHOSTTY_TERMINAL_OPT_RENDER_HOLD` を登録し、hold の間は render state を写し直さない（時限 1 秒）
- ✅ **T-075**（2026-11-11）→ `crates/session`（`et-session`）に移し、embterm.exe と module の両方が使う。`apps/embterm/src/session.rs`（端末 + PTY + 読み書きのスレッド + 知らせ）を、Emacs module と共有するクレート（`et-session`）に移す。T-028 の前にやる
- ✅ **T-076**（2026-11-11: 前半は済み。閉じる待ちは別スレッドで行い、終わったら `Closed` を知らせる。`kill-buffer` は 1 ms 程度で返る。後半 → 閉じ方を「CTRL_CLOSE_EVENT → 5 秒まで待つ → Job ごと止める」にした。**`.swp` が残るのは vim の仕様**（MSYS の vim は SIGHUP で常に残す、Windows 版は書き換えたときだけ残す）で強制終了ではなかった。`kill-emacs` では `embterm-kill-emacs-timeout`（既定 0.5 秒。利用者が了承）だけ待つ。t054 §3.8）閉じる処理を UI スレッドから外す（`Pty::close` は最大数秒待つ。module では Emacs を止めてしまう）。
  あわせて、閉じたときに vim の `.swp` が残る（実質の強制終了）ので、子に先に終了を促す手順を考える
- ✅ **T-077** 変わった行だけ描き直すか（今は毎回全体を描き、`Present(1)` で垂直同期を待つ）→ T-068 の測定で決める（2026-11-11）→ **毎回全体を描くで確定**（ふつうの画面 0.2〜1 ms、最悪 8 ms。フリップモデルの差分を追う複雑さに見合わない）
- ✅ **T-078** 自動確認の道具をリポジトリに入れる（キーを送るスクリプト、画面の `.txt` を期待値と比べる回帰テストを `cargo xtask` から）（2026-11-12）→ `cargo xtask test-screens [--bless]`。`test/screens/cases.txt` のケースを embterm.exe で動かし、画面のテキストを期待値と比べる（cmd 3 + vttest 38）。docs/notes/t078-tests.md
- ✅ **T-079** 150% の DPI で確かめる（2026-11-11）→ Emacs の中で確認した（T-043）。embterm.exe 単体では確かめていない
- ✅ **T-080** バッファ名の付け方（2026-11-11）→ `embterm-buffer-name-function`。長いパスはファイル名だけ（`*embterm: cmd*`）、タイトルが無ければコマンド名、`embterm-buffer-name-max-length`（40）で切る
- ✅ **T-081** Emacs での自動確認を `cargo xtask` から呼ぶ（2026-11-12 → `cargo xtask test-emacs [名前...]`。11 個を順に動かし、終了コードとログの NG で合否。7 分ほど。マウスは各試験が PostMessage で送っている。docs/notes/t078-tests.md）（`cargo xtask dist` → `emacs -Q -l test/smoke.el` → ログの確認。T-078 と合わせる）。
  子 HWND のホイール・ドラッグでの選択・Emacs 側に来たホイールの転送も `PostMessage` で送って確かめる
- ✅ **T-082** 子プロセスは最初にウィンドウに出したときに起動する（大きさが決まってから起こすため）。表示しないまま起動する用途（バックグラウンドでコマンドを走らせる）が要るかを決める（2026-11-12）→ 要る。`embterm-run` の DISPLAY がどのウィンドウにも出さなければ（`ignore` など）`embterm-background-size`（80x24）で起動し、出したらウィンドウに合わせる。docs/notes/t078-tests.md
- ✅ **T-083** tr-ime の再変換（2026-11-11）→ 端末のバッファでは再変換と前後参照のフックをバッファローカルに差し替え、空文字列を通知する（`delete-char` を走らせない）。tr-ime が無ければ何もしない。**手元の Emacs での確認は未**（単体の確認のみ）
- ✅ **T-084** ドラッグがペインの外へ出たときの自動スクロール（2026-11-12）→ 外にある間 50 ms ごとにペインをスクロールし、端の行までドラッグを伸ばす（外へ出た距離で 1〜5 行）。docs/notes/t078-tests.md
- ✅ **T-085** 複数行の貼り付けの確認: ブラケットペーストを有効にしていないアプリ（pwsh の PSReadLine など）では毎回確認が出る。確認を止める設定か、PSReadLine を区別する扱いを決める（2026-11-12）→ 今のまま（利用者が選んだ。Windows Terminal の既定と同じ）。止めるなら `embterm-confirm-unsafe-paste` を nil
- ✅ **T-086** copy モードで折り返しを繋げる（unwrap）。とても大きなスクロールバックを分割して渡す（2026-11-12）→ バッファの行は端末の行のまま、折り返しの改行に `embterm-wrap` を付け、コピー（`filter-buffer-substring`）でつなぐ。テキストは 1 MiB ずつ渡す（1.17 MB で Emacs の止まりは最大 23 ms）。docs/notes/t078-tests.md、`test/copy.el`
- ✅ **T-087** ウィンドウを入れ替えたとき（`window-swap-states`、tab-bar のタブの切り替えなど）のスクロール位置の引き継ぎ（T-044 で tab-bar でも失われることを確かめた）（2026-11-12）→ 端末が移ったらペインを付け替え、消えたウィンドウのペインは 4 つまで隠して取っておく（tab-bar は元のウィンドウを生き返らせる）。docs/notes/t078-tests.md
- ✅ **T-088** Kitty graphics の描画（2026-11-11）→ render state が placeholder・相対配置・層を解決するので自前の走査は不要だった。PNG は WIC でデコード。層の順・切り取り・ビットマップのキャッシュ。split・狭いペイン・遡ったペインでも崩れない。docs/notes/t088-kitty-graphics.md、`test/kitty-graphics.py`、`test/kitty.el`
- ✅ **T-089** 非 ASCII の文字 + 修飾キー（`C-あ` `M-あ`）を送るか決める（今は束縛が無く端末に届かない）（2026-11-12）→ M- だけ送る（利用者が選んだ）。ESC の下の表に文字の範囲で束縛。conhost が Alt を捨てるので ESC を前に送る。KKP は `CSI 12354;3u`。C-あ は束縛できないので送らない。docs/notes/t078-tests.md、`test/meta.el`
- ✅ **T-090** KKP の安全策の限界: 求めたアプリの子孫が長生きすると戻せない。`CSI <99u` は今の画面だけに効く（2026-11-12）→ 自動では見分けられないので限界として残し、手で戻す `embterm-reset-keyboard` を足した。docs/notes/t078-tests.md
- ✅ **T-091** 手元での再確認（2026-11-11）→ ユーザーが確認: 日本語の確定、絵文字、claude の `Shift+Enter`、`C-c C-c`、PSReadLine のキー、yazi の png / svg / pdf のプレビュー。個別の報告が無いもの: `Esc`、JP 配列の `C-@`、AltGr、マウスの形
- ✅ **T-092** Kitty graphics のアニメーション（`animation_tick` と次の時刻のタイマー。いまは最初のフレームだけ）（2026-11-12）→ 描く前に `animation_tick` を呼び、次のフレームの時刻にタイマー。3 色の画像で色が順に変わることを確かめた。docs/notes/t078-tests.md
- ✅ **T-093** 画像の保存量の上限の defcustom 化（いまは 128 MiB）と、`t=t` / `t=s`（`OpenFileMappingW`）への対応（yazi の `t=s` は大きな画像で base64 より速い）（2026-11-12）→ `embterm-image-storage-limit`、`embterm-kitty-temp-file-directory`（指定したときだけ `t=t`）。`t=s` は libghostty-vt が Windows で未対応なので見送り（yazi は base64 で動く）。docs/notes/t078-tests.md
- ✅ **T-094** 画像を含む行のコピーに placeholder の文字（`U+10EEEE`）が混じるので除く（2026-11-12）→ `format_plain` で placeholder と後ろの結合文字を除く。docs/notes/t078-tests.md
- ✅ **T-095** 親の端末の環境変数（`WEZTERM_*`、`TERM_PROGRAM_VERSION`）が子に伝わる件。`et_pty` で落とすか決める（yazi の判定は XTVERSION で上書きされるので害は無い）（2026-11-12）→ 落とす（`WEZTERM_*`・`WT_SESSION`・`VSCODE_*` など。`setenv` したものは残る）。`TERM_PROGRAM_VERSION` は EmbTerm の版。docs/notes/t078-tests.md
- ✅ **T-096** 大きな画像の PNG のデコードが端末のロックの中で走る時間を測る（T-054）（2026-11-11）→ 4000x4000 でデコード 33〜38 ms・`vt_write` 58〜65 ms がロックの中。Emacs には響かない。ロックの外に出す方法は libghostty-vt の API に無いので記録で終える。重いのは UI スレッドで GPU に載せるところ → T-105
- ⏸ **T-097** embterm.exe のウィンドウが一瞬画面いっぱい（`319x75`）にリサイズされて戻るログが数回あった。原因を調べる（2026-11-11: 26 回で再現せず。大きさの変化・`showCmd`・`WM_SYSCOMMAND` をログに出すようにしたので、次に起きたらそれで原因を分ける）
- ✅ **T-098** 他のアプリが前面のとき、端末のペインをクリックしても Emacs にフォーカスが入らない（2026-11-11）→ フレームが前面でなければ `WM_MOUSEACTIVATE` に `MA_ACTIVATE` を返すようにした。本物のクリックでの動作をユーザーが確認した
- ✅ **T-099** 通知を Windows のトースト通知にする（いまは `message`）（2026-11-12）→ 既定 `embterm-notification-default`: Emacs が裏ならトースト（10 秒で消す）、前なら `message`。本物のトーストの見え方は手で確かめる。docs/notes/t078-tests.md
- ✅ **T-100** 進捗（OSC 9;4）をモードラインに出す（2026-11-12）→ ` [42%]` などを出す（`embterm-show-progress`）。docs/notes/t078-tests.md
- ✅ **T-101** リンクの上にマウスがあるときに下線や URI を出す（2026-11-12）→ 同じ URI の桁に下線、URI をエコー領域に（`embterm-show-link-on-hover`）。ペインから出たら消す。docs/notes/t078-tests.md
- ✅ **T-102** テーマを切り替えたらアプリにダーク / ライトを知らせる（`CSI ?996n`、mode 2031）（2026-11-12）→ 既定の背景色の明るさで決め、`CSI ?996n` に答え、mode 2031 のアプリには変わったときに知らせる。docs/notes/t078-tests.md、`test/scheme.el`
- ✅ **T-103** `embterm-project` のバッファ名にプロジェクト名を入れる（2026-11-12）→ `*embterm: EmbTerm: pwsh*`。docs/notes/t078-tests.md
- ✅ **T-104** 何もしていないときの Lisp のポーリングの CPU（端末 15 個で 4〜5%）。隠れている端末の巡回を軽くする、何も変わらない間は間隔を延ばす（t054 §7-1）（2026-11-12）→ module が `open_channel` のパイプで変化を知らせ、Lisp はそれを受けて読む。見回りは 0.5 秒ごと。15 個で 6.1% → 0.3〜1.6%、クリックの反応 12 → 1 ms。docs/notes/t078-tests.md、`test/idle.el`
- ✅ **T-105** 大きな画像を GPU に載せる UI スレッドの仕事（4000x4000 で 60〜140 ms）。表示の大きさに縮めてから載せる、変換をワーカーでする、ロックの中の画素の写しを `Arc` の共有にする（T-096 の続き）（2026-11-12）→ 表示の大きさまで縮め、大きな画像は別のスレッドで変換する。UI スレッドの描画の最大 3.75 ms。ロックの中の写し（27 ms）は libghostty-vt の API の範囲では減らせない。docs/notes/t078-tests.md、`test/bigimage.el`
- ⏸ **T-106** 入力から画面に出るまでを DWM の合成込みで実測し、Windows Terminal と比べる（カメラ・フォトセンサ）（2026-11-12: カメラ・フォトセンサが要るので保留）
- ⏸ **T-107** 本物のデバイスの消失（TDR、ドライバの更新）での作り直しを確かめる（`dxcap -forcetdr` など）（2026-11-12: 実機の GPU をリセットし作業中の全アプリが一瞬止まるので、利用者が都合のよいときに。擬似的な消失は T-067 で確認済み）
- ❌ **T-108** `box: ┌──┬──┐` の `─` が描かれていないように見える（SGR の確かめの画面。T-054 の変更の前後で同じ）。overlap の既知の見え方（T-013）か別の問題かを調べる（2026-11-11 → 不具合ではない。30pt で撮ると `┌──┬──┐`・`x─x`・`━`・`═`・`╭─╮` はすべて描かれていた。12pt では角と横線がつながって見分けにくいだけ）
- ✅ **T-109** 子の環境変数を Emacs の `process-environment` から作る（2026-11-11）→ 起動時の環境からの差分（設定・削除）を module に渡す。`setenv` が子に届く（`make-process` と同じ）。`setenv` のユニバイトの値は `locale-coding-system` で戻す。`test/env.el`
- ✅ **T-110** コマンド・作業ディレクトリ・環境・バッファ名を指定して開く `embterm-run`（2026-11-11）→ `(embterm-run COMMAND &key DIRECTORY ENVIRONMENT NAME DISPLAY)`。`my:claude-term` の置き換え（T-059）に使う
- ❌ **T-111**（2026-11-12 に廃止 → T-120）普段使いの置き場に入れる `cargo xtask install [DIR]`（2026-11-11）→ 既定 `C:\Apps\embterm`。使用中の conpty.dll / OpenConsole.exe / exe は `.old-<時刻>` に退避して入れ替える。`target/dist` を普段の Emacs に読ませると dist と試験が上書きに失敗するため
- ✅ **T-112** 文字の点滅（SGR 5）を描く。カーソルの点滅（T-035）と同じタイマーで（T-055 で見つけた。低）（2026-11-12）→ 点滅の文字が見えているときだけ 600 ms ごとに切り替える（カーソルとは別のタイマー）。docs/notes/t078-tests.md
- ⏸ **T-113** `CSI 0 S` / `CSI 0 T` を libghostty-vt が 1 行として扱わない（何もしない）件を上流に知らせるか決める（T-055）（2026-11-12: 送るかは利用者が決める。下書き docs/notes/t113-upstream-draft.md）
- ✅ **T-114** vttest の自動確認を回帰テストにする（T-078 と合わせる。T-055 の道具はリポジトリに入れていない）（2026-11-12）→ T-055 で合格した 38 画面を `cargo xtask test-screens` のケースにした。期待値は T-055 で見て確かめた画面と一致。docs/notes/t078-tests.md
- ✅ **T-120** 普段の Emacs への入れ方を、~/.emacs.d の側から写す形にする（2026-11-12）→ `~/.emacs.d` の `M-x my:embterm-update`（`my-embterm.el`）が
  このリポジトリの `target/dist/` を `~/.emacs.d/embterm/`（git の管理外）に写す（`C-u` で先に `cargo xtask dist`）。中身が同じものは写さず、
  使用中の `conpty.dll` / `OpenConsole.exe` は `.old-<時刻>` に名前を変えてから写す（次に写すときに消える）。`cargo xtask install` は削除した。
  `target/dist` を直接読ませない理由は変わらない（使用中のファイルがロックされ dist と試験が失敗する）。手順は `~/.emacs.d` の README の「EmbTerm の端末」。
  `~/.emacs.d/etc/pwsh-emacs-shell.ps1` がシェル統合（`embterm/etc/embterm.ps1`）を読み込む。写す処理は、写した先の embterm.exe を動かして
  conpty.dll と OpenConsole.exe を使用中にした状態で確かめた
- ⏸ **T-115** Windows Terminal と vttest を手で見比べる（DECCOLM・倍幅の行。T-055 では自動の比較を省いた）（2026-11-12: 手で見る作業なので利用者に。合格した画面は T-114 の回帰テストにした）
- ✅ **T-116** 子の作業ディレクトリのドライブレターを大文字にする（2026-11-11）→ Emacs は `~` の展開や `directory-file-name` で小文字に落とす。claude は cwd を `.claude.json` のプロジェクトのキーに使うので、Emacs から開いたものと端末から開いたものでプロジェクトが割れる。プロンプトもほかの端末と同じ `C:\` になる
- ✅ **T-117** 子に `PWD` / `OLDPWD` を渡さない（2026-11-11）→ Git Bash などから起動した Emacs の `PWD` が残り、starship（nyagos・pwsh のプロンプト）と yazi が実際の作業ディレクトリより `PWD` を信じて、Emacs を起動したディレクトリで開いたように見えた。`~/.emacs.d` の `M-x nyagos` / `yazi` を作っていて見つけた

## 9. macOS への移植（T-121 の段階）

この一式は複数の節（描画・PTY・入力・IME・試験・配布）にまたがるので、§8 ではなくここにまとめる。
前提と見積もりは [research/macos-port.md](research/macos-port.md)。**やるとは決めていない**（T-122 の測定を見て決める）。

- ⬜ **T-122**（判断）mac の ghostel の遅さの出どころを測る。redisplay / Lisp スレッドの詰まり / 入力の往復 / PTY の読みの間隔のどれか。
  **redisplay か Lisp の詰まりなら EmbTerm の効果は確実、入力の往復が主因なら差は小さい**（macos-port.md §7）。ここで着手するかを決める
- ⬜ **T-123**（設計）`cfg` とトレイト（`Pty` / `Renderer` / ビューの置き場）での platform 分岐。**リポジトリは分けない**（Windows が常用なので二重管理は避ける）。
  Windows 側を壊さない保険は既存の回帰試験（`cargo xtask test-screens`、`cargo xtask test-emacs`）。この refactor は Windows で先に行い、試験を通してから macOS の実装を足す
- ⬜ **T-124**（段階 0）`et-vt` を macOS でリンクし、`et-pty` の Unix 版を書く。VT だけのヘッドレスで `cargo xtask test-screens` を通す
  - libghostty-vt は上流に `ghostty-vt-1.0.0.xcframework.zip` がある。**zig を入れずに済むかを先に確かめる**（済まなければ `zig -Dtarget=aarch64-macos`）
  - `crates/vt/src/png.rs`（WIC）を ImageIO か `png` クレートに差し替え
  - `et-pty`: `openpty` + `TIOCSWINSZ` + プロセスグループ + `SIGHUP` → 猶予 → `SIGKILL` + `waitpid`。再配布版の探索・Job Object・コマンドラインの引用・環境ブロックは不要
  - **ここで失敗すれば一番早く分かる。半日で「心臓部は macOS で動く」まで行けるので、最初にやる**
  - 通ったら requirements.md §3（「macOS / Linux は対象外【案】」）と §8 を書き換える
- ⬜ **T-125**（段階 1）CoreText + Metal のレンダラと、NSWindow 1 枚の `embterm`（デバッグ用 exe）
  - グリフのアトラス方式（Ghostty / WezTerm / Alacritty と同じ）。Windows の「アトラスは要らない」（T-008 / T-077）は D2D の `DrawGlyphRun` が速かったからで、macOS には同じ近道が無い
  - 背景は Windows と同じ考え（セル 1 つ = 1 画素）でインスタンスの矩形にできる。`frame.rs`・eaw の overlap / shrink（T-013）・画像の層は移植する
  - `Present(1)` の待ち（T-068）に当たるのは `CAMetalLayer` の drawable の待ち。**描画スレッドで待ち、main スレッド（= Emacs）は止めない**
  - 画面比較の回帰試験（`test-screens` の 38 + 3 画面）で正しさを担保し、T-054 と同じ最悪の画面（314x70）で負荷を測る
- ⬜ **T-126**（段階 2）Emacs module: `EmacsView` への埋め込み・位置合わせ・キー・ポーリング・イベント
  - Win32 のメッセージループと「端末ごとの UI スレッド」を「main スレッド（Lisp から直接呼ぶ）+ 描画スレッド」に置き換える。
    `PostMessage` の列は不要になるが、**ビューのジオメトリは main スレッドだけで触る**
  - フレーム → NSWindow の対応付け（macos-port.md §3.7）: タイトルを一時的に一意にして引き、`window-id` をキーに覚える
  - マウスは案 X（子ビューで受け、`acceptsFirstResponder` NO でフォーカスは奪わない）
  - `keys.rs` から win32-input-mode と `VkKeyScan` 系を外し、macOS のキー（Option = Meta、Command）を足す
- ⬜ **T-127**（段階 3）IME・マウス・選択 / コピー・Kitty graphics・OSC・通知・シェル統合
  - **IME**: 端末のバッファでは `ns-put-working-text`（`ns-insert-working-text` / `ns-echo-working-text`、変数 `ns-working-text`）を奪い、
    変換中の文字列を module に渡して端末のカーソル位置に自前で描く。候補ウィンドウの位置は `-[EmacsView firstRectForCharacterRange:]` 次第で、
    ポイントの移動か method swizzling のどちらか（macos-port.md §5-2）
  - 通知は `UNUserNotification` か `osascript`（T-099 の Windows のトーストに当たる）。クリップボードは NSPasteboard か Lisp 経由
  - 画像の共有メモリ（`t=s`。T-093 で Windows では見送った）が使える見込み
  - `etc/embterm.bash` はそのまま使える（OSC 51;E / OSC 133。T-118 / T-119）
- ⬜ **T-128**（段階 4）測定と自動確認の移植
  - mac の ghostel と比べる（T-122 で測った項目と同じ道具で）。入力から表示までと、大量の出力の最中の Emacs の止まり
  - `test-emacs`（11 本）の移植。Windows は `PostMessage` でマウスを送っていた（T-081）が、macOS は合成イベントに権限が要るので module に試験用の注入口を作る
