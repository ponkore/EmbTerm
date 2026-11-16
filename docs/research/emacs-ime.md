# Emacs 31.1 の w32 の IME の扱い

2026-11-11 / TODO T-024 の記録（T-009 / T-010 の材料）

## 0. この文書の読み方

- 根拠は `file:line` で示す。パスの略記は次のとおり。
  - `w32fns.c` などの `src/...` → `C:\Apps\emacs\src\emacs-31.1\src\...`
  - `tr-ime/...` → `~\.emacs.d\straight\repos\tr-emacs-ime-module\...`
  - `w32-ime.el` → `~\.emacs.d\straight\repos\w32-ime.el\w32-ime.el`
  - `my-japanese.el` → `~\.emacs.d\user-lisp\my-japanese.el`
- **【読】** はソースから読んで確かめたこと、**【推】** は推測（Win32 / IME の一般知識や読みからの見込み）で、実測が要るもの。
- Emacs は起動していない。ここに書いた挙動はすべて未実測である。

## 1. 要点

| 問い | 答え |
|---|---|
| 変換中の文字列（preedit）を誰が描くか | **IME 自身のウィンドウが描く。** Emacs 31.1 素でも tr-ime advanced でも、Emacs はバッファに preedit を描かない【読】 |
| 子 HWND の下に隠れるか | **隠れない見込み。** IME の変換ウィンドウはフレームの子ではないポップアップなので、子 HWND より上に出る【推】 |
| 変換ウィンドウの位置 | **Emacs がカーソル（ポイント）を描くたびに**、その位置を `ImmSetCompositionWindow(CFS_RECT)` で IME に渡す【読】 |
| 候補ウィンドウの位置 | Emacs も tr-ime も `ImmSetCandidateWindow` を呼ばない。IME が変換ウィンドウの位置から決める【読＋推】 |
| module から位置を決める方法 | ①ポイントを端末のカーソルに当たる位置へ動かす（素の Emacs で可）②**tr-ime の `tr-ime-modadv--set-composition-window` で画素で指定**（ユーザーの環境で可、最も正確）③`WM_IME_CONTROL` を IME の既定ウィンドウに `SendMessage`（別スレッドから可だが Emacs の再表示で上書きされる） |
| 確定文字列の届き方 | **1 文字ずつの文字キーイベント**（`MULTIBYTE_CHAR_KEYSTROKE_EVENT`）。文字列としては届かない【読】 |
| ユーザーの設定 | **tr-ime advanced + w32-ime**（`default-input-method` = `"W32-IME"`）【読】 |
| 結論 | **案 A で IME は成り立つ見込みが高い。** 難所は「位置合わせ」と「tr-ime の再変換・前後参照が隠れたバッファを触ること」。案 B は IMM32 / TSF を自前で持つことになり、Emacs との IME 状態の共有とキーの戻しが重い |

## 2. Emacs 31.1 素の IME の扱い

### 2.1 スレッドの構成【読】

- w32 の Emacs は **Lisp のスレッド（メイン）** と **ウィンドウのスレッド（`w32_msg_worker`）** に分かれている。
  フレームの HWND はウィンドウのスレッドが作り、`w32_wnd_proc` もそこで動く
  （`w32term.c:191` `dwWindowsThreadId`、`w32term.c:8204-8206` `CreateThread (… w32_msg_worker …)`、
  `w32fns.c:3930-3950`、`w32fns.c:2759` `CreateWindow (EMACS_CLASS …)`）。
- IME まわりの処理（`WM_IME_*` の受け取り、`ImmSetCompositionWindow`、`ImmSetOpenStatus`）は
  **すべてウィンドウのスレッドで行う作り**になっている。Lisp のスレッドからは `PostMessage` /
  `PostThreadMessage` で頼むだけ（下の 2.3・2.6）。

### 2.2 使っている IMM32 の関数【読】

- `imm32.dll` から動的に読む関数は次の 6 つだけ（`w32fns.c:193-201`、`w32fns.c:246-251`、`w32fns.c:12322-12334`）。
  - `ImmGetCompositionStringW`、`ImmGetContext`、`ImmReleaseContext`、`ImmSetCompositionWindow`、
    `ImmGetOpenStatus`、`ImmSetOpenStatus`
- **`ImmSetCandidateWindow`・`ImmSetCompositionFont` は使っていない。** `WM_IME_COMPOSITION` の
  処理も無い（`src/` と `lisp/` を grep して該当なし）。TSF も使っていない（IMM32 だけ）。

### 2.3 変換ウィンドウの位置の決め方【読】

流れは次のとおり。

1. 再表示でカーソルを描くとき `w32_draw_window_cursor` が呼ばれ、**選ばれているウィンドウのカーソル
   （`active_p`）なら**、カーソルのフレーム内の画素位置を大域変数に書く
   （`w32term.c:6670-6683`）。
   - `w32_system_caret_x = WINDOW_TEXT_TO_FRAME_PIXEL_X (w, w->phys_cursor.x)`
   - `w32_system_caret_y = WINDOW_TO_FRAME_PIXEL_Y (w, w->phys_cursor.y) + ascent の差`
   - `w32_system_caret_window = w`、ヘッダ行・モードラインの高さも記録
2. 続けて **`PostMessage (hwnd, WM_IME_STARTCOMPOSITION, 0, 0)` を自分のフレームに投げる**
   （`w32term.c:6685`）。変換を始めていなくても、カーソルを描くたびに投げる。
   あわせてシステムキャレットも動かす（`w32term.c:6696` → `w32fns.c:5876-5887` `SetCaretPos`）。
3. ウィンドウのスレッドの `w32_wnd_proc` が `WM_IME_STARTCOMPOSITION` を受け、
   `COMPOSITIONFORM` を **`CFS_RECT`** で組んで `ImmSetCompositionWindow` を呼ぶ
   （`w32fns.c:5029-5081`）。
   - `ptCurrentPos` = 上の `w32_system_caret_x/y`（＝ポイントの位置）
   - `rcArea` = ウィンドウの本文領域（左端〜右フリンジの手前、ヘッダ行の下〜モードラインの上）
   - 選ばれているウィンドウが記録時と違う・ウィンドウが消えた、のときは何もしない（`w32fns.c:5050-5072`）
4. そのあと必ず `DefWindowProc` に回す（`w32fns.c:5083-5089` の `goto dflt`）。
   これで IME 既定の変換ウィンドウが動く。

補足:

- **`cursor-type` が nil でも位置は渡る。** `get_window_cursor_type` は選ばれているウィンドウでは
  `*active_cursor = true` のまま `NO_CURSOR` を返し（`xdisp.c:34647`、`xdisp.c:34689-34691`）、
  `display_and_set_cursor` はそれでも `draw_window_cursor` を呼ぶ（`xdisp.c:35208-35211`）。
  `w32_draw_window_cursor` の `active_p` の分岐は `cursor_type` と関係なく走る（`w32term.c:6670`）。
  よって**端末のバッファでカーソルを消していても、ポイントの位置が IME に伝わる**。
- 位置が更新されるのは **Emacs がカーソルを描き直したとき**だけである。ポイントが動かず再表示も
  無ければ、変換ウィンドウの位置は変わらない。
- 座標はフレームの HWND のクライアント座標。子 HWND はその上に重なっているので、
  端末のセルの画素位置（子 HWND 内）＋子 HWND のフレーム内の位置、で同じ座標系になる。

### 2.4 変換中の文字列（preedit）を誰が描くか【読＋推】

- 【読】Emacs は `WM_IME_STARTCOMPOSITION` を `DefWindowProc` に回し（`w32fns.c:5089`）、
  `WM_IME_COMPOSITION` は処理しない（＝既定処理）。`WM_IME_ENDCOMPOSITION` も既定処理に回す
  （`w32fns.c:5091-5093`）。**Emacs は preedit の文字列を一度も読まない**
  （`GCS_COMPSTR` を読むコードが無い。読むのは確定の `GCS_RESULTSTR` だけ、`w32fns.c:5008-5010`）。
- 【推】既定処理に回したアプリでは、IME が自前の **変換ウィンドウ（いわゆる near-caret / レベル 2）**
  を `ptCurrentPos` の位置に出して preedit を描く。このウィンドウは IME の既定ウィンドウが持つ
  **トップレベルのポップアップ**で、Emacs のフレームの子ではない。Win32 の z オーダーでは
  フレームの子 HWND（`HWND_BOTTOM` に置いても置かなくても）より常に上に出る。
  → **子 HWND の下に隠れない見込み。**
- 【推】TODO の T-010 に書いた「変換中の文字列は Emacs がバッファに描く」は **W32-IME には当たらない**。
  バッファに描くのは Emacs Lisp の input method（quail の `japanese`、ddskk など）のオーバーレイで、
  それなら確かに子 HWND の下に隠れる。ユーザーは W32-IME なのでこの問題は起きない。

### 2.5 確定した文字列の届き方【読】

1. IME が確定すると `WM_IME_CHAR` が文字の数だけ来る。Emacs は最初の 1 通で
   `ImmGetCompositionStringW (GCS_RESULTSTR)` を読み、**1 文字（UTF-16 の 1 単位）ごとに
   `WM_UNICHAR` として Lisp のスレッドに渡す**。残りの `WM_IME_CHAR` は数えて捨てる
   （`w32fns.c:4990-5026`、`ignore_ime_char` は `w32fns.c:271-272`）。
2. 修飾キーは既定で落とす（`w32-ignore-modifiers-on-IME-input` = t、`w32fns.c:5003-5006`、
   `w32fns.c:11899-11906`）。
3. `w32_read_socket` が `WM_UNICHAR` を 1 文字ずつ入力イベントにする。サロゲートペアは組み立てる。
   種類は `ASCII_KEYSTROKE_EVENT` か `MULTIBYTE_CHAR_KEYSTROKE_EVENT`
   （`w32term.c:5273-5386`、とくに `w32term.c:5313-5330`・`w32term.c:5385`）。

→ **Lisp から見ると、確定文字列は「文字のキーが N 回押された」のと同じ**。キーマップで
`self-insert-command` 相当を受ければ 1 文字ずつ来る。文字列としてまとめて受ける口は無い。

- 変換中のキー（ローマ字の打鍵、変換、確定の Enter など）は `VK_PROCESSKEY` になり、
  Emacs はイベントにしない（`w32fns.c:4806-4808`）。**端末のキーマップは変換中のキーを見ない**ので、
  案 A で「変換中の Enter が端末に送られる」心配は無い【読】。

### 2.6 IME の ON / OFF【読】

- `w32-get-ime-open-status`: 選ばれているフレームの HWND の HIMC を Lisp のスレッドから読む
  （`w32fns.c:10771-10798`）。
- `w32-set-ime-open-status`: `WM_EMACS_IME_STATUS` を **ウィンドウのスレッドに `PostThreadMessage`**
  し（`w32fns.c:10801-10814`）、そちらで **`GetFocus ()` の HWND** の HIMC に `ImmSetOpenStatus`
  する（`w32fns.c:3794-3806`）。
  - 対象が「フレーム」ではなく「フォーカスのある HWND」である点に注意（案 B に効く。5 章）。

### 2.7 lisp/international【読】

- `lisp/international/` に W32-IME 専用の処理は無い。`w32` を含むのは `characters.el`・`fontset.el`・
  `mule-cmds.el`・`mule.el` で、文字幅・フォント・コーディングの話だけ。
  `W32-IME` という input method は Emacs 本体ではなく **w32-ime.el が登録する**（`w32-ime.el:295-296`）。

## 3. ユーザーの設定（tr-ime advanced + w32-ime）

### 3.1 何を使っているか【読】

| 項目 | 値 | 根拠 |
|---|---|---|
| パッケージ | `w32-ime`（2020-11 版）と `tr-ime`（0.5.0、tr-emacs-ime-module `063aee5`）を straight で導入 | `my-japanese.el:197-212` |
| モード | **advanced**（`tr-ime-advanced-install`） | `my-japanese.el:251` |
| DLL | `straight/build/tr-ime/tr-ime-modadv-2-x86_64-w64-mingw32.dll` | ファイルの存在 |
| input method | `default-input-method` = `"W32-IME"`、`w32-ime-initialize` | `my-japanese.el:254`、`my-japanese.el:268` |
| IME の状態の表示 | カーソル色（ON で green、OFF で white）、モードラインに `[あ]` | `my-japanese.el:265`、`my-japanese.el:286-294` |
| バッファ切替で IME を引き継ぐ | `w32-ime-buffer-switch-p` = nil | `my-japanese.el:297` |
| ミニバッファで IME を OFF | `minibuffer-setup-hook` で `deactivate-input-method` | `my-japanese.el:308-309` |
| tr-ime の個別設定 | 無し（`custom.el` にも無い）→ すべて既定値 | grep |

tr-ime advanced の既定で有効になるもの（`tr-ime/lisp/tr-ime.el` の `tr-ime-advanced-initialize` と各
`defcustom`）: サブクラス化、IME 状態の通知とフック、`ime-font`、isearch 中の位置合わせ
（`tr-ime-isearch-p` = t、`tr-ime/lisp/tr-ime-isearch.el:167`）、プレフィックスキー検出
（`tr-ime/lisp/tr-ime-prefix-key.el:60`）、状態の同期、**再変換**（`tr-ime-reconversion.el:88`）、
**前後参照変換 DOCUMENTFEED**（`tr-ime-documentfeed.el:73`）。

### 3.2 tr-ime advanced の仕組み【読】

- DLL が Emacs のウィンドウのスレッドに **`WH_GETMESSAGE` フック**を入れ
  （`tr-ime/src/get_msg_hook.cc:50`）、そこで Emacs のフレームを **`SetWindowSubclass` でサブクラス化**する
  （`tr-ime/src/get_msg_proc.cc:47-84`）。サブクラス化はスレッドをまたげないので、フックで
  ウィンドウのスレッドに入り込んでいる。
- 対象は **クラス名 `Emacs` で親を持たない HWND だけ**（`tr-ime/src/get_msg_proc.cc:163-190`、
  `GetParent` があれば除外は `:167`）。**EmbTerm の子 HWND はサブクラス化されない。**
- サブクラスのプロシージャが扱うメッセージ（`tr-ime/src/subclass_proc.cc:775-829`）:
  `WM_KEYDOWN`（プレフィックスキー）、`WM_IME_NOTIFY`（ON/OFF の通知）、`WM_IME_REQUEST`（再変換・前後参照）、
  `WM_IME_COMPOSITION`（再変換の後始末）、`WM_IME_STARTCOMPOSITION`（フォントと**位置の上書き**）、
  `WM_IME_ENDCOMPOSITION`。

### 3.3 preedit を誰が描くか（tr-ime advanced）【読＋推】

- 【読】`WM_IME_COMPOSITION` は再変換の後始末をするだけで、あとは `DefSubclassProc`
  （＝Emacs の `w32_wnd_proc` → `DefWindowProc`）に回す（`tr-ime/src/subclass_proc.cc:664-683`）。
  preedit の文字列を読むのは DOCUMENTFEED で IME に前後の文脈を返すときだけ（`:399-438`）で、描かない。
- 【読】`WM_IME_STARTCOMPOSITION` では `ime-font` があれば `ImmSetCompositionFontW` でフォントを
  変える（`:700-715`）。
- → **素の Emacs と同じく、preedit は IME の変換ウィンドウが描く。** フォントだけ Emacs に寄せられる。
  子 HWND の下に隠れない見込みも同じ【推】。
- 【読】tr-ime standard（`tr-ime-mod`）は Emacs 28 以降では DLL を読まない
  （`w32-get-ime-open-status` があれば不要、`tr-ime/lisp/tr-ime.el` の `tr-ime-standard-initialize`）。
  standard でも preedit の描き方は素の Emacs と同じ。

### 3.4 位置を上書きする口がある（重要）【読】

- `tr-ime-modadv--set-composition-window HWND STYLE X Y LEFT TOP RIGHT BOTTOM`
  （`tr-ime/src/lisp-in-cc.cc:508-571`）。`COMPOSITIONFORM` を組み、フレームに登録メッセージ
  `WM_TR_IME_SET_COMPOSITIONWINDOW` を `SendMessage` する（`:568`）。
- サブクラス側はそれを **スレッドごとの `compform_`** に覚え（`tr-ime/src/subclass_proc.cc:57`、`:318-328`）、
  以後の `WM_IME_STARTCOMPOSITION` では **Emacs の処理（2.3 の 3）を呼ばずに**
  `ImmSetCompositionWindow (compform_)` してから `DefWindowProc` に回す（`:722-755`）。
  STYLE が 0 なら上書きをやめて Emacs の位置に戻る（`lisp-in-cc.cc` の docstring）。
- tr-ime 自身は isearch 中に preedit をエコーエリアに出すためにこれを使っている
  （`tr-ime/lisp/tr-ime-isearch.el:104-143`）。**isearch を抜けると STYLE 0 に戻す**（`:137-143`）。
- Emacs はカーソルを描くたびに `WM_IME_STARTCOMPOSITION` を投げる（2.3 の 2）ので、
  **`compform_` を書き換えれば次の再表示で IME の位置に反映される**。

### 3.5 端末に効く tr-ime の副作用【読＋推】

| 機能 | 何をするか | 端末での影響 |
|---|---|---|
| プレフィックスキー検出 | `C-x`・`C-h`・`C-c`・`ESC` を押すと IME を一時 OFF、次のコマンドの前に戻す（`tr-ime-prefix-key.el:60`、`subclass_proc.cc:590-612`、`:161-191`） | 端末に `C-c`・`ESC` を送るたびに IME が一瞬 OFF→ON する。害は無い見込みだが確かめる【推】 |
| 再変換（RECONVERSION） | IME の再変換キーで、**カレントバッファのポイントの行**を IME に渡し（`tr-ime-reconversion.el:47-59`）、確定後に **`backward-char` / `delete-char` をカレントバッファで実行**（`lisp-in-cc.cc:766-792`） | 端末のバッファ（隠れていて、たぶん read-only）を壊す・エラーにする。**端末のバッファでは止めるか、端末の行を渡す形に差し替える必要がある**【推】 |
| 前後参照（DOCUMENTFEED） | 変換のたびにポイントの行を IME に渡す（`tr-ime-documentfeed.el`、`subclass_proc.cc:556-588`） | 隠れたバッファの行が文脈になる。害は小さい。**端末の今の行を渡せば変換精度が上がる**余地【推】 |
| isearch の位置合わせ | isearch の終わりに位置の上書きを 0 に戻す（`tr-ime-isearch.el:137-143`） | EmbTerm が上書きを使うなら、isearch の後に**上書きし直す**必要がある【読】 |

フックは `run-hooks` で呼ばれる（`lisp-in-cc.cc:736-763`）ので、**バッファローカルのフック値で
差し替えられる**【推。`run-hooks` の通常の挙動から】。ただしフックが何も通知しないと C++ 側は
タイムアウトまで待つ（`subclass_proc.cc:193-243`、`:375-397`）ので、空文字列を通知する形が無難【推】。

## 4. 案 A（フォーカスは Emacs に残す）での IME

### 4.1 そのままで成り立つこと【読＋推】

- フォーカスはフレームの HWND にあるので、IME の入力コンテキストは Emacs のもの。ON/OFF、
  `C-\`、漢字キー、w32-ime のモードライン、カーソル色のフックは**今までどおり動く**【読：2.6・3.1】。
- 変換中のキーは Emacs に来ない（2.5）。端末のキーマップは確定文字だけを受ける【読】。
- preedit と候補は IME のポップアップに出て、子 HWND の下に隠れない見込み【推：2.4】。

### 4.2 位置合わせの方法（module 側から指定する余地）

| 方法 | 前提 | 精度 | 手間・難所 |
|---|---|---|---|
| **A-1: ポイントを動かす** | 素の Emacs で可 | 隠れたバッファの文字の並びと Emacs のフォントで決まる。**端末のセルとずれる** | 端末のカーソル（行・桁）に当たるバッファ位置にポイントを置く。行の高さ・文字幅が端末と違うと画素がずれる。`(space :align-to (PX))` 等の display 指定で x は画素で合わせられるが、y（行の高さ）を合わせるのは難しい【推】 |
| **A-2: tr-ime の上書き** | tr-ime advanced（ユーザーの環境） | **画素で正確** | 端末のカーソルのセル → 子 HWND 内の画素 → フレームのクライアント座標に直して `tr-ime-modadv--set-composition-window` を呼ぶ。**端末のウィンドウを離れたら STYLE 0 に戻す**、isearch の後に入れ直す。`compform_` はスレッド（＝全フレーム）で 1 つ【読：3.4】 |
| A-3: `WM_IME_CONTROL` | 素の Emacs で可 | 画素で正確だが**持続しない** | `SendMessage (ImmGetDefaultIMEWnd (frame), WM_IME_CONTROL, IMC_SETCOMPOSITIONWINDOW, &cf)`。IME の既定ウィンドウは Emacs のウィンドウのスレッドのものなので、**別スレッドからでも所有スレッドで実行される**（tr-ime standard が ON/OFF に同じ手を使う、`tr-ime/src/tr-ime-mod.c:61-73`）。ただし Emacs が次にカーソルを描くと 2.3 の処理で**ポイントの位置に戻される**【読＋推】 |
| A-4: module が自分でサブクラス化 | 素の Emacs で可 | 画素で正確 | tr-ime と同じく `WH_GETMESSAGE` フックで Emacs のウィンドウのスレッドに入り、フレームをサブクラス化して `WM_IME_STARTCOMPOSITION` を横取りする。tr-ime と二重になる。**重いので最後の手段**【推】 |

推し: **A-2 を主、A-1 を tr-ime が無いときの代わり**にする。A-1 でも「行は合う・桁が少しずれる」程度なら
実用になるかもしれないので、まず A-1 の誤差を測る。

### 4.3 別スレッドからの IMM32 呼び出しの制約【読＋推】

- 【読】Emacs は IME の位置・ON/OFF の変更を**わざわざウィンドウのスレッドで**行っている
  （位置は `PostMessage` した `WM_IME_STARTCOMPOSITION`、ON/OFF は `PostThreadMessage`、
  `w32term.c:6685`、`w32fns.c:10812`）。tr-ime も `SendMessage` でサブクラス（＝ウィンドウのスレッド）に
  頼む（`lisp-in-cc.cc:568`）。
- 【推】IMM32 の設定系の関数（`ImmSetCompositionWindow`・`ImmSetOpenStatus` など）は、
  **別スレッドが持つ入力コンテキストに対しては失敗する**作りと考えられる（ReactOS の imm32 にも
  そのための検査がある）。EmbTerm の module（Lisp のスレッド）や子 HWND のスレッドから、フレームの HIMC に
  直接 `ImmSetCompositionWindow` するのは**当てにしない**。実測で確かめる（6 章 P4）。
- したがって module から位置を変える経路は、**Lisp から tr-ime の関数を呼ぶ（A-2）**か、
  **`SendMessage` でウィンドウのスレッドに頼む（A-3・A-2 の登録メッセージを module から直接送る）**のどちらか。
  module のスレッドから Emacs のウィンドウのスレッドへ同期で送るなら `SendMessageTimeout` にする
  （GenPreview の約束どおり、`~/.emacs.d/docs/embed/hwnd-child-window.md:222-224`）。

### 4.4 位置を更新するタイミング【推】

- 位置は **変換を始める前**に合っていればよい。変換中は端末のカーソルはふつう動かない（アプリは確定まで
  何も受け取らない）。
- 端末のカーソルが動いたら（PTY の出力を描いた後）、module → Lisp の通知（T-014 のポーリング）で
  Lisp 側が A-1 / A-2 を行う。ポーリングの間隔ぶん遅れるが、打ち始めの瞬間に合っていれば足りる。
- **claude がプロンプトの入力位置に本物のカーソルを置いているかは分からない。** Ink 系の TUI は
  カーソルを隠して（DECTCEM off）自前で反転文字を描き、本物のカーソルを別の場所に置くことがある。
  その場合は「最後に見えていたカーソル位置」などで代わりを決める必要がある。**実測が要る**（P6）。

### 4.5 確定文字列を module に送る【読＋推】

- 【読】文字は 1 つずつ別のイベントで来る（2.5）。端末のキーマップで `self-insert-command` を
  remap するか `[t]` で受け、1 文字ずつ module に送れば足りる。修飾キーは落ちてくる（2.5 の 2）。
- 【推】N 文字の確定で N 回コマンドループが回る。PTY への書き込みも N 回になる。claude なら問題ない
  見込みだが、まとめたいなら、受けたコマンドの中で `input-pending-p` の間 `read-event` で続きの文字を
  吸ってから 1 回で送る。ブラケットペーストで包むのは**しない**（IME の確定は貼り付けではない）。
- 【推】IME の ON/OFF は端末のカーソルにも出したい（Emacs のカーソルは子 HWND の下で見えない）。
  `input-method-activate-hook` / `input-method-deactivate-hook`（ユーザーはカーソル色に使用、
  `my-japanese.el:288-294`）や `w32-ime-on-hook` / `w32-ime-off-hook`（`w32-ime.el:99-103`）で module に知らせる。
  `C-\` を通さない漢字キーでの切り替えは tr-ime の通知（`tr-ime-modadv--setopenstatus-hook`、
  `subclass_proc.cc:614-642`）→ `tr-ime-sync` 経由で input method の状態に反映される【読】。

## 5. 案 B（子 HWND がフォーカスを取る）での IME

### 5.1 必要になること【推】

| 項目 | 内容 |
|---|---|
| IME の受け口 | 子 HWND で `WM_IME_STARTCOMPOSITION` / `WM_IME_COMPOSITION` / `WM_IME_ENDCOMPOSITION` / `WM_IME_NOTIFY` / `WM_IME_REQUEST` を自分で扱う。最低限は「`ImmSetCompositionWindow` で位置を渡して既定処理に回す」（レベル 2。自スレッドの HIMC なので制約なし）。preedit を端末の中に描くなら `GCS_COMPSTR`・`GCS_COMPATTR`・`GCS_CURSORPOS` を読み、`ImmSetCandidateWindow` も自分で呼ぶ（レベル 3）。いちばん良いのは TSF（Windows Terminal と同じ）で、実装は重い |
| IME の状態 | 子 HWND が別スレッドなら、**入力コンテキスト（HIMC）が Emacs と別**になる。ON/OFF が Emacs と共有されない。w32-ime のモードライン・カーソル色・`C-\` と食い違う。合わせるなら、フォーカスを移すたびに Emacs 側の状態を読んで自スレッドで `ImmSetOpenStatus` する |
| `w32-set-ime-open-status` | Emacs のウィンドウのスレッドで `GetFocus ()` の HWND を対象にする（`w32fns.c:3796-3804`）。入力キューが連結されていると **子 HWND が返り、別スレッドの HIMC を触って失敗する**おそれ |
| tr-ime | 子 HWND はサブクラス化の対象外（`get_msg_proc.cc:167`）。プレフィックスキー・再変換・状態の通知は効かない。`tr-ime-modadv--getopenstatus` はフレームの HIMC を見るので子 HWND の状態とずれる |
| Emacs のフォーカス | フレームが `WM_KILLFOCUS` を受け、システムキャレットを捨てる（`w32fns.c:5665-5679`）。Emacs はフレームがフォーカスを失ったと見なす（カーソルが中抜きになる、`focus-out` のフック、tr-ime-font の `after-focus-change-function` など） |
| Emacs に戻すキー | `C-x`・`M-x`・ウィンドウ移動などを Emacs に渡す道が要る。(a) フレームに `WM_KEYDOWN` を `PostMessage`（Emacs の `w32_wnd_proc` は修飾キーを `GetKeyState` で読むので、合っているかは実測次第）、(b) 子 HWND がフォーカスをフレームに返してから `SendInput` で打ち直す（順序が崩れやすい）、(c) キーを Lisp に通知して `unread-command-events` に積む（module → Lisp はポーリングなので遅れる）。どれも `C-x` の後の 2 打目以降をどちらが受けるかの切り替えが要る |
| Win キー | Emacs の低レベルキーボードフックは `GetFocus ()` に向けて Win キーを投げ直す（`w32fns.c:2870-2873`、`:2965-2966`）。子 HWND に届く可能性がある |

### 5.2 案 B の利点【推】

- preedit を端末のセルに合わせて描ける（レベル 3 / TSF）。位置も子 HWND の中で完結し、Emacs の
  再表示・tr-ime の状態に左右されない。単体デバッグ用の exe（requirements §5.11）とも同じ実装になる。

## 6. 比較と結論

### 6.1 比較

| 観点 | 案 A（Emacs に残す） | 案 B（子 HWND が取る） |
|---|---|---|
| IME の ON/OFF・モードライン・カーソル色 | **そのまま動く** | Emacs と別の状態になる。同期を自作 |
| preedit の描画 | IME のポップアップ（隠れない見込み） | 自前（レベル 3 / TSF）か IME のポップアップ |
| 変換ウィンドウの位置 | A-2（tr-ime）なら正確。A-1 は誤差あり。Emacs の再表示に依存 | 自スレッドで `ImmSetCompositionWindow` / `ImmSetCandidateWindow`。正確 |
| 候補ウィンドウの位置 | IME 任せ（変換ウィンドウに付いてくる） | 自分で指定できる |
| 確定文字列 | 1 文字ずつのキーイベント → module | `WM_IME_CHAR` / `GCS_RESULTSTR` を直接 PTY へ |
| tr-ime の副作用 | 再変換・前後参照が隠れたバッファを触る → 端末のバッファで差し替え | 無し（tr-ime は子 HWND を触らない） |
| Emacs のキー（`C-x` など） | Emacs のキーマップがそのまま | 戻す仕組みが要る（最大の難所） |
| 実装量 | 小〜中 | 大 |

### 6.2 結論【推】

- **案 A で IME は成り立つ見込みが高い。** T-010 の心配のうち「preedit が子 HWND の下に隠れる」は
  W32-IME では起きない見込みで、残る課題は**位置合わせ**だけになる。
- 位置合わせは **tr-ime の `tr-ime-modadv--set-composition-window` を使う（A-2）** のが最も素直。
  ユーザーの環境には tr-ime advanced がある。tr-ime が無いときは**ポイントを動かす（A-1）**。
- 端末のバッファでは **tr-ime の再変換・前後参照のフックをバッファローカルで差し替える**。
- 案 B は IME だけを見ても、状態の同期・tr-ime との食い違い・Emacs へのキーの戻しが重く、
  T-009 で案 A を採る理由がまた 1 つ増える。

## 7. まず実測で確かめること（プローブの手順案）

前提: GenPreview と同じ形の**ダミーの子 HWND**（単色で塗るだけ、WS_CHILD・WS_EX_NOACTIVATE・
MA_NOACTIVATE・HWND_BOTTOM）を Emacs のウィンドウの本文領域に重ねる。ユーザーの設定（tr-ime advanced）のままと、
`emacs -Q`（素の W32 IME。input method を使わず IME を直接 ON）の両方で行う。

| # | 確かめること | 手順 | 見るもの |
|---|---|---|---|
| P1 | preedit・候補が子 HWND の上に出るか | 子 HWND を重ね、IME を ON にして「にほんご」と打ち、変換・候補一覧まで出す | 見た目。Spy++ 等で変換ウィンドウのクラス名・オーナー・トップレベルかどうか |
| P2 | 位置がポイントに付いてくるか（A-1） | 隠れたバッファの中でポイントを左上・右下・途中に動かしてから打つ。`cursor-type` nil でも試す | 変換ウィンドウの位置が動くか、`posn-at-point` の画素との差 |
| P3 | tr-ime の上書き（A-2） | `(tr-ime-modadv--set-composition-window (string-to-number (frame-parameter nil 'window-id)) 1 X Y L T R B)` を子 HWND 内の任意のセル位置で呼び、`(redisplay t)` してから打つ | 指定した画素に出るか。再表示が無いときに反映が遅れるか。isearch の後に戻されるか |
| P4 | 別スレッドからの IMM32（4.3） | module のスレッド・子 HWND のスレッドから `ImmGetContext(frame)` → `ImmSetCompositionWindow`、および `SendMessage(ImmGetDefaultIMEWnd(frame), WM_IME_CONTROL, IMC_SETCOMPOSITIONWINDOW, &cf)` | 戻り値・`GetLastError`。効くか、Emacs の再表示で戻されるか |
| P5 | 確定文字列の届き方（4.5） | 端末用のキーマップで `[t]` を受けるコマンドを作り、`last-command-event` と時刻をログに出す。「日本語入力」と確定 | 1 文字 1 コマンドか、修飾キー、`input-pending-p` でまとめられるか |
| P6 | claude の本物のカーソルの位置（4.4） | ghostel か Windows Terminal で claude を動かし、プロンプトで待っているときの出力を記録（`script` 相当・ConPTY のログ） | DECTCEM（`?25l/h`）と CUP の位置。入力位置に本物のカーソルがあるか |
| P7 | tr-ime のプレフィックスキー（3.5） | IME ON のまま端末のバッファで `C-c`・`ESC` を押す | 端末に届くか、IME が ON に戻るか |
| P8 | 再変換・前後参照（3.5） | 端末のバッファ（read-only）で確定済みの文字の上で変換キー、また変換中の挙動 | エラー・バッファの変更・待ち時間。フックのバッファローカル差し替えで止まるか |
| P9 | 案 B の確認（参考） | ダミーの子 HWND で `SetFocus` してから IME を ON/OFF、`w32-get-ime-open-status` と比べる。`w32-set-ime-open-status` を呼ぶ | HIMC が別か、状態がずれるか、Emacs のフォーカスの扱い |

P1・P3・P5 が通れば、T-010 は「案 A + tr-ime の上書き」でほぼ決められる。P6 の結果は位置の決め方
（本物のカーソルか、別の手掛かりか）に効くので早めに行う。
