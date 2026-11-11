# GenPreview の埋め込みの実装と、その後に分かったこと

- 日付: 2026-11-11
- TODO T-018（T-026 / T-030 / T-031 の材料を含む）の記録
- 読んだもの（GenPreview は `~/Projects/GenPreview`、以下 `GP/` と書く。HEAD は `2432604`）
  - `GP/docs/emacs-embedded-view.md`（設計案と段階ごとの結果）、`GP/docs/adr/0002`・`0006`・`0008`、
    `GP/docs/architecture.md` §8.1・§8.2・§8.3・§13
  - `GP/crates/window/src/{embed,window,app,system,trace}.rs`、`GP/crates/render/src/{gpu,surface,brushes}.rs`、
    `GP/crates/render/src/flow/worker.rs`
  - `GP/apps/genpreview-emacs/{Cargo.toml,build.rs,src/lib.rs,src/engine.rs}`、`GP/lisp/genpreview.el`、`GP/xtask/src/main.rs`
  - `GP/spikes/embed-surface/`（手順 0 のスパイク）、`GP/tools/emacs-view-test*.el`
  - git log の `d6a3332`（段階 A）〜 `2432604`（2026-11-10〜11 の 10 コミット）
  - `~/.emacs.d/docs/embed/hwnd-child-window.md`（プローブの記録。以下「プローブ」）
  - `emacs` クレート 0.23.0 のソース（`~/scoop/persist/rustup/.cargo/registry/src/index.crates.io-*/emacs-0.23.0/`）
- GenPreview のソースは変更していない。ビルドも Emacs の起動もしていない（数値はすべて GenPreview の記録から引いた）

---

## 0. 要点

| EmbTerm の課題 | GenPreview から分かったこと | EmbTerm への示唆 |
|---|---|---|
| **T-008 描画** | **フリップモデル（`FLIP_SEQUENTIAL`）のスワップチェーンを Emacs の子ウィンドウで使っても真っ黒にならない**（作成と破棄 50/50、リサイズ・分割・最小化・付け替えも正常。`GP/spikes/embed-surface/result.txt`）。真っ黒はプロセスをまたぐ埋め込み（`prevhost.exe`）だけの現象だった | Direct2D + フリップモデルで進めてよい。ただし**確かめたのは「1 回塗る」だけ**で、毎秒数十回の描き直しは誰も測っていない。`Present(1, …)`（垂直同期待ち）が UI スレッドを止める量は要測定 |
| **T-006 スレッド** | **ビューア 1 枚 = UI スレッド 1 本。Lisp → `PostThreadMessage`＋頼みごとの列、ビューア → Lisp は `Mutex` の共有状態を 100 ms ごとにポーリング。** UI スレッドで 5 ms を超える処理（レイアウト・ラスタライズ・読み直し）はすべてワーカーへ出した | 形はそのまま使える。端末では **PTY の読み・VT の解釈を UI スレッドから必ず外し**、UI スレッドは「画面の写しを取って描く」だけにする。端末の状態は HWND と別の寿命にする（フレームを消すと HWND とスレッドが道連れになる） |
| **T-009 キー** | **フォーカスは常に Emacs。** 子ウィンドウは `WS_EX_NOACTIVATE` と `MA_NOACTIVATE` でフォーカスを取らない。キーは minor mode のキーマップで受けてコマンドとして送る。**cua-mode に負けるので `emulation-mode-map-alists` の先頭に置き、ポーリングのたびに取り返す** | 案 A（Emacs に残す）の実績はここにある。ただし GenPreview が送るのは十数個の単純なキーだけで、IME の変換中の文字列は扱っていない（T-010 は未踏） |
| **T-063 split** | **同じバッファを 2 つのウィンドウに出しても、ビューアは 1 枚だけ**（選択中 → 前に出していた → 最初のウィンドウ）。ほかのウィンドウは下のバッファのテキストが見えるだけ。複数フレームでも同じ規則で `SetParent` で付け替える | **「ウィンドウごとに別のスクロール位置」の前例は無い。** EmbTerm は GenPreview より一歩先が要る。下の §4.2 |
| **T-015 共有** | 埋め込みの核（`embed.rs` 569 行 + `window.rs` の一部 + `gpu.rs`/`surface.rs` 166 行 + Lisp 約 390 行）は小さいが、`gp-window`・`gp-render` は `gp-core`（文書のモデル）に深く依存している | **path 依存や共通クレート化ではなく、必要なファイルをコピーして作り替える**のが妥当（§8） |

---

## 1. 実装の現状

### 1.1 段階ごとの到達点

設計案（`GP/docs/emacs-embedded-view.md` §9）の段階は **0・A・B・C のすべてが実装済み**。
2026-11-10 に既定の表示になった（`1c21348`、ADR 0008）。設計案の冒頭のステータス（`GP/docs/emacs-embedded-view.md:4`
「段階 A まで実装済み」）は更新されておらず古い。

| 段階 | 内容 | 状態 | コミット |
|---|---|---|---|
| 0 | 子ウィンドウでスワップチェーンが黒くならないか、UI スレッドの重さを測る | **完了**（§1.2） | `d6a3332` |
| A | `Host::Embedded`、ビューアのスレッド、共有状態、module の `view-*` 12 関数、`genpreview.el` の位置合わせ・キー・isearch・ポーリング | **完了** | `d6a3332` |
| B | PDF のラスタライズと Flow（Markdown）のレイアウトをワーカーへ。PDF の isearch、`C-g` でスクロールを戻す | **完了** | `753d8fe`、`9e41a8d`、`4a6890d`、`bc097f7` |
| C | 同じバッファを複数のウィンドウ、別フレームへの付け替え、テーマの切り替え、検索バー | **完了**（複数ウィンドウは「1 枚だけ」で決着、検索バーは描かない） | `5aaa982` |
| 既定化 | `genpreview-view` の既定を t。作れなかったら従来の表示に落とす | 完了 | `1c21348` |
| その後 | Ctrl+クリックのリンク、cua-mode との衝突、`C-s` をグローバルに替えている人への対処 | 完了 | `9e2b3e2`、`f8a509f`、`2432604` |

### 1.2 手順 0「フリップモデルを子ウィンドウで使って大丈夫か」の結果

**大丈夫だった。** `GP/spikes/embed-surface`（`lib.rs` 435 行。Emacs の子ウィンドウを作り、塗って、画面の色を
`GetPixel` で読む）で 3 方式を比べた（`GP/docs/emacs-embedded-view.md:224-240`、`GP/spikes/embed-surface/result.txt`）。

| 方式 | 50 回の作成と破棄 | 作成から最初の描画まで（中央値） | リサイズ 5 回・分割・最小化から復帰・別フレームへ付け替えて戻す |
|---|---|---|---|
| **`flip`（`gp_render::Surface`、`DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`）** | **50 / 50 正常** | 45 ms | すべて正常 |
| `discard`（BitBlt モデル） | 50 / 50 正常 | 43 ms | すべて正常 |
| `hwnd`（`ID2D1HwndRenderTarget`） | 50 / 50 正常 | 67 ms | すべて正常 |

- Phase 4 ② の「3 回に 1 回真っ黒」（`GP/docs/architecture.md:622-624`）は**プロセスをまたぐ親子**（`prevhost.exe`）での
  現象で、**同じプロセスの中でスレッドをまたぐ親子（Emacs の module）では再現しなかった**。原因の特定まではしていない
- 測るときは `emacs -Q` で背景を白にした（実際の設定の modus-vivendi は背景が `#000000` で、「合成されずに黒い」と区別できない）
- 「最初の描画まで」の 45 ms には、ビューアごとの `Gpu::create`（D3D11 + D2D デバイスの作成）が入っている
- 結論として**今の `Surface`（`GP/crates/render/src/surface.rs`、102 行）をそのまま使っている**

**EmbTerm にとっての注意（推測を含む）**

| 点 | 内容 |
|---|---|
| 測ったのは「塗る」だけ | スパイクは単色で塗るだけ。**連続して描き直す負荷・`Present` の待ち・入力の遅れは測っていない** |
| `Present(1, 0)` | `surface.rs:99` は垂直同期を 1 回待つ。`BufferCount: 2`（`surface.rs:34`）なので、連続して描くと **UI スレッドが最大 1 フレーム（約 16 ms）止まる**（推測）。UI スレッドは Emacs と入力キューが連結されているので、描画の嵐の中でクリックすると Emacs のキーが待たされうる。端末では `Present(0, …)`、待機可能なスワップチェーン（`DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT`）、フレームレートの上限のどれかを検討する（T-008 で測る） |
| DPI | Emacs は system-aware なので、150% のモニタでは子ウィンドウごと DWM に引き伸ばされてぼやける（プローブ「結果 2」）。Direct2D で描いても鮮明にならない。端末の文字には痛い（T-043） |

### 1.3 実装して新たに分かったこと・ハマったこと

| # | 事象 | 原因 | 対処 | 出典 |
|---|---|---|---|---|
| 1 | **ビューアの下端に黒い帯**（PDF で 18 px） | スクロールバーが消えるとクライアント領域が広がるが、その `WM_SIZE` は `relayout` の最中（`APP` の `RefCell` を借りている間）に**同期で再入**して届き、`wndproc` が取りこぼす。スワップチェーンが古い大きさのまま残る | **描く直前にクライアント領域とスワップチェーンの大きさを比べ、違えば合わせ直す** | `app.rs:308-321`、`d6a3332` |
| 2 | 位置合わせで `WM_SIZE` を取りこぼす | `SetWindowPos` は `WM_SIZE` を同期で送り直す | **`Place`/`Hide`/`Reparent`/`Close` は `App` を借りずに処理する** | `embed.rs:435-465` |
| 3 | `wndproc` の再入 | `SetWindowTextW`・`SetWindowPos` は `wndproc` を同期で呼び直す | `try_borrow_mut` に失敗したら `DefWindowProcW` に任せる（Phase 0 からの作法） | `window.rs:381-389` |
| 4 | **UI スレッドの重い処理が Emacs のキーを止める** | 入力キューの連結。詰まっている最中にビューアをクリックすると、その後ろの Emacs のキー入力も待つ | ワーカーへ出す。**PDF のズーム + クリック → F24 が 66.4 ms → 8.1 ms、Markdown が 36.8 ms → 3.6 ms**（基準 2.5〜3.3 ms） | `GP/docs/emacs-embedded-view.md:328-335` |
| 5 | ワーカーで作った D2D 資源を UI スレッドで描く | D2D のファクトリが `MULTI_THREADED` なら、同じ `ID2D1Device` から**スレッドごとにデバイスコンテキスト**を作ればよい。ブラシやビットマップはデバイスの資源なので共有できる。ただし「作ったときのデバイスコンテキストに描く」コードはワーカー側に描いてしまった | 描く先を「いま `DrawTextLayout` しているデバイスコンテキスト」（スレッドローカル）にした | `flow/worker.rs:6-9`、`4a6890d` |
| 6 | **OS のライト / ダークの切り替えが届かない** | `WM_SETTINGCHANGE` は**トップレベルのウィンドウにしか配られない**。子ウィンドウには来ない。`HWND_MESSAGE` のメッセージ専用ウィンドウも対象外 | ビューアのスレッドに**見えないトップレベルのウィンドウ**（`WS_POPUP`、`WS_EX_TOOLWINDOW`）を作り、受けた通知を `SendMessage` で取り次ぐ | `window.rs:270-317`、`5aaa982` |
| 7 | 親のフレームを消すとビューアも消える | 子ウィンドウは親と一緒に破棄される → `WM_DESTROY` → `PostQuitMessage` で**ビューアのスレッドも終わる** | ポーリングが `:alive` = nil に気づき、表示中なら作り直す（**スクロール位置は先頭に戻る**） | `genpreview.el:1023-1031`、`GP/docs/emacs-embedded-view.md:413` |
| 8 | リンクを開くと `emacs.exe` が起動しかける | 埋め込みでは `current_exe()` が `emacs.exe` | 開くのはイベントにして Emacs に任せる | `app.rs:921-936` |
| 9 | 絶対パスのリンクが `browse-url` に行った | `C:\…` のドライブ名がスキームに見えた | Lisp 側で `file-name-absolute-p` を先に見る | `9e2b3e2` |
| 10 | **cua-mode が有効だと `C-v`/`M-v` が裏のバッファをスクロール** | cua-mode は `scroll-up-command` を `cua-scroll-up` に付け替え、その付け替えは `emulation-mode-map-alists` にあるのでマイナーモードの remap に勝つ。remap は 1 段しかたどられないので付け替え返せない | **ビューアのキーマップを `emulation-mode-map-alists` の先頭に置き、ポーリングのたびに先頭を取り返す** | `genpreview.el:1010-1013, 1156-1171`、`f8a509f` |
| 11 | `C-s` をグローバルに `consult-line` にしていると検索できない | 裏のバッファでポイントが動くだけ | モードのキーマップで `C-s`/`C-r` を isearch に戻す | `genpreview.el:230-238`、`2432604` |
| 12 | モードラインの `%` が消えた | `mode-line-process` の文字列はもう一度書式として解釈される | `%` を 2 重にエスケープ | `genpreview.el:1070-1074` |
| 13 | `Ctrl`+クリックの判定 | ビューアのスレッドから `GetKeyState(VK_CONTROL)` で読めた（入力キューが連結されているので Emacs 宛ての修飾キーの状態が共有される） | — | `GP/docs/emacs-embedded-view.md:405-406`、`window.rs:405, 484` |
| 14 | PDF の isearch の位置が改行の数だけずれた | ビューアのテキストは CRLF、バッファは LF | バッファに入れる文字列を UTF-16 の列から作り、落とした CR も位置の対応表で埋める（`Utf16Map`） | `embed.rs:124-177`、`9e41a8d` |

---

## 2. スレッド構成

### 2.1 全体の形

```text
Emacs の Lisp スレッド                 module（同じプロセス）                  ビューアのスレッド（1 枚に 1 本）
──────────────────────               ─────────────────────────              ─────────────────────────────────
genpreview.el                         EmbeddedView { thread_id, Arc<Shared> }   子 HWND（WS_CHILD, 親 = Emacs のフレーム）
  フック → view-place  ───────────▶   Shared.requests に積む                   GetMessageW のループ
  キー   → view-command                + PostThreadMessageW(WM_APP_REQUEST) ─▶  列を全部取り出して apply
  isearch → view-highlight                                                     thread_local! APP: RefCell<Option<App>>
  タイマー 100 ms ─ view-state ◀──────  Shared.state を clone して返す   ◀──   描くたびに publish_view_state
                 ─ view-events ◀──────  Shared.events を take              ◀──   Clicked / Link / Reloaded / Error
  w キー ─ view-selected-text ◀ 唯一の同期（mpsc + recv_timeout 200 ms）
                                                                            ワーカー: GPU 作成 / 読み込み / 読み直し /
                                                                            PDF ラスタライズ / Flow レイアウト
                                                                            → PostMessage(WM_APP_*) で UI に戻す
```

| 要素 | 実装 | 出典 |
|---|---|---|
| スレッドの作成 | `thread::Builder` で名前付きスレッドを立て、**子ウィンドウを作るところまでは待つ**（`recv_timeout(5 s)`、プローブで 3.4 ms）。読み込みは待たない | `embed.rs:262-279, 317-334` |
| 1 スレッド 1 ビューア | `thread_local! static APP: RefCell<Option<App>>`。単体ウィンドウ版と同じ | `window.rs:46-49` |
| Lisp → ビューア | `Request` の列（`Place`/`Hide`/`Reparent`/`Command`/`Highlight`/`Select`/`ScrollTo`/`SelectedText`/`Close`）を `Mutex<Vec>` に積み、`PostThreadMessageW` で起こす。**メッセージ 1 通で溜まった列を全部さばく** | `embed.rs:111-122, 196-201, 366-371` |
| ビューア → Lisp | `Shared { state: Mutex<ViewState>, events: Mutex<Vec<Event>>, … }`。Lisp は**ロックを取って写すだけ**。描くたびに `publish_view_state` で更新 | `embed.rs:52-109, 494-520`、`app.rs:363-365` |
| 唯一の同期 | 選択範囲のテキストだけは UI スレッドに聞く。`mpsc` で返事を受け、200 ms で諦めて nil | `embed.rs:242-247`、`genpreview.el:1119-1124` |
| ポーリング | 100 ms。**ビューアが 1 枚でも生きている間だけ**タイマーを動かす（いなくなればフックとタイマーを外す） | `genpreview.el:893-894, 947-963, 1010-1021` |
| `open_channel` を使わない理由 | CRT を静的リンクした module は Emacs のファイル記述子を自分の CRT の `_get_osfhandle` に渡すことになり、**Emacs ごと落ちる**（0xC0000409） | ADR 0006 |
| ワーカー → UI | ワーカーは `PostMessageW(hwnd, WM_APP_*)` で知らせる。D2D のビットマップにするなど**デバイスに触る仕上げは UI スレッド** | `app.rs:380-390`、`window.rs:32-40, 531-534` |
| 依頼の間引き | Flow のレイアウトは「最後の 1 つだけ」を持ち、古い結果は世代で捨てる。PDF は見えているページだけをまとめて頼み直す | `flow/worker.rs:10`、`GP/docs/emacs-embedded-view.md:314-320, 350` |

### 2.2 GPU（D3D/D2D デバイス）の共有

- **設計案では「module で 1 つ作って共有する」**（`GP/docs/emacs-embedded-view.md:71-72`）だったが、
  **実装はビューアごとに `Gpu::create` している**（`embed.rs:337`）。Raster 表示の側は別に 1 つ持つ（`engine.rs:50-71`）
- `Gpu` は COM のインターフェースだけの `Clone` な構造体で、`unsafe impl Send`（`gpu.rs:13-24`）。
  D2D のファクトリは `D2D1_FACTORY_TYPE_MULTI_THREADED`、DirectWrite は `DWRITE_FACTORY_TYPE_SHARED`（`gpu.rs:35-38`）
- **1 つのデバイスを複数スレッドで使う実績は「UI スレッドとそのワーカー」まで**（スレッドごとに `CreateDeviceContext`。`flow/worker.rs:6-9`）。
  ビューア同士で 1 つのデバイスを分け合う形は動かしていない
- デバイスを失ったら、そのビューアが自分で `Gpu::create` からやり直す（`app.rs:532-547`）。ワーカーは古いデバイスを持つので作り直す
- デバイスの作成は「数十〜百 ms」（`gpu.rs:27`）

**EmbTerm への示唆**: 端末を十数個開く前提（要件 §6.2）なら、**デバイスはプロセスで 1 つにして共有する**ほうがよい
（作成時間とメモリ）。そのときは「デバイスを失ったら全員で作り直す」仕組み（世代番号など）が要る（推測。GenPreview に前例なし）。

### 2.3 端末（毎秒数十回の描き直し）の観点で使えるか

| 観点 | GenPreview | 端末での評価 |
|---|---|---|
| 描き直しの頻度 | 入力・スクロール・ズームのときだけ。`InvalidateRect` → `WM_PAINT` → `paint` | 出力のたびに描く。**`WM_PAINT` 駆動にフレームの上限（例: 60 fps、まとめ描き）を足す**必要がある |
| UI スレッドの仕事 | 5 ms を超えたら外へ（トレースに出す。`bc097f7`） | **PTY の読み（ブロックする）と VT の解釈は別スレッド。** UI スレッドは「画面の写しを取って描く」だけにする。これは約束 1 の直接の帰結 |
| 共有状態 | `Mutex<ViewState>` を Lisp が 100 ms ごとに写す | 端末の画面（セルの格子）をロックして写すのは UI スレッド。VT スレッドとのロックの粒度（ダブルバッファ、ダーティ行）は T-006 で決める（推測） |
| テキストの描き方 | Flow は段落ごとに `IDWriteTextLayout` を作る（レイアウトが重いのでワーカーへ出した） | **毎フレーム `IDWriteTextLayout` を作るのは重すぎる見込み**（推測）。グリフのキャッシュ（`DrawGlyphRun`、アトラス）を T-008 で検討。`brushes.rs`（色ごとのブラシのキャッシュ、29 行）はそのまま使える |
| 知らせ | 100 ms のポーリングで足りた（クリック → `select-window` が最大 100 ms 遅れる） | タイトル・ベル・終了・OSC 7 なら 100 ms で足りる。**キー入力と表示は Lisp を通らない**（キー → module → PTY、PTY → VT → 描画）ので、ポーリングの間隔は入力の遅れに効かない |
| スレッドメッセージ | `PostThreadMessageW`（`hwnd` = NULL） | **モーダルループ（スクロールバーのつまみのドラッグ、`WS_VSCROLL` を付けている。`window.rs:255`）の最中に投げたスレッドメッセージは捨てられる**（Win32 の既知の挙動。GenPreview では未観測で、列は残るので次の通知で回収される。推測）。EmbTerm では**HWND かメッセージ専用ウィンドウ宛ての `PostMessage`** にするほうが安全 |

---

## 3. キーとフォーカス・マウス

### 3.1 フォーカスを Emacs に残す方式

| 仕掛け | 内容 | 出典 |
|---|---|---|
| 作成時のスタイル | `WS_CHILD \| WS_CLIPSIBLINGS \| WS_VSCROLL \| WS_HSCROLL`、`WS_EX_NOACTIVATE \| WS_EX_NOPARENTNOTIFY`。作ったら `HWND_BOTTOM` | `window.rs:246-268` |
| クリックでフォーカスを取らない | `WM_MOUSEACTIVATE` に `MA_NOACTIVATE`。**`App` ができる前（読み込み中）にも効くよう、`APP` を見る前に返す** | `window.rs:376-380` |
| IME | ビューアの HWND から IME の文脈を外す（`ImmAssociateContextEx(…, 0)`）。フォーカスを持たないので実際には効く場面が無い | `window.rs:167-168`、`system.rs:63-71` |
| キーの経路 | フォーカスが Emacs にあるので `WM_KEYDOWN` は来ない。来ても埋め込みでは検索バーを開かない | `app.rs:616-624` |
| DPI | 埋め込みでは `SetProcessDpiAwarenessContext` を呼ばない（Emacs のプロセスの設定を変えてしまう） | `GP/docs/emacs-embedded-view.md:88`、`window.rs:60-62`（単体ウィンドウ版だけ） |

### 3.2 キーマップの作り方

- `genpreview-view-mode`（minor mode）の `genpreview-view-mode-map`（`genpreview.el:1126-1154`）。
  1 文字のキー（`n`/`p`/`SPC`/`<`/`>`/`+`/`-`/`t` …）と、**`<remap>`** で `next-line`・`scroll-up-command`・`beginning-of-buffer`・
  `mwheel-scroll`・`pixel-scroll-precision` などを差し替える
- 各キーは `(genpreview-module-view-command VIEW "scroll-down")` を呼ぶだけのコマンド。マクロで量産（`genpreview.el:1084-1110`）。
  名前は Rust 側の `gp_core::command::Command` の `FromStr` に合わせる（`lib.rs:326-332`）
- **cua-mode との兼ね合い**（`f8a509f`）
  - cua-mode は `cua-enable-cua-keys` が nil でも `scroll-up-command` → `cua-scroll-up` の付け替えを `emulation-mode-map-alists` に置く。
    この層は minor mode のキーマップより強い
  - `genpreview--view-emulation-alist` = `((genpreview-view-mode . genpreview-view-mode-map))` を **`emulation-mode-map-alists` の先頭**に入れる。
    あとから読み込まれたパッケージに前を取られるので、**モードを有効にするたびと 100 ms のポーリングのたびに先頭へ戻す**（`genpreview.el:1010-1013, 1165-1171`）
- 親のメジャーモードのキーマップでは、グローバルに替えられがちな `C-s`/`C-r` を isearch に戻す（`genpreview.el:230-238`）

**EmbTerm への示唆（T-009）**

- 端末は「ほとんどすべてのキーを送り、少数（`C-x`、`M-x`、ウィンドウ移動など）だけ Emacs に残す」ので、
  GenPreview の「十数キーを奪う」より**ずっと広いキーマップ**になる。**`emulation-mode-map-alists` の先頭に置く手法はそのまま要る**
  （cua-mode の `C-c`/`C-x`/`C-v`、evil、`which-key` などのより強い層に勝つため）。eat の semi-char / char モードのような
  キーマップを差し替える切り替えも、この層の中で行うことになる（推測）
- GenPreview は**文字の入力も IME も扱っていない**（isearch はミニバッファで入力するため）。変換中の文字列が子 HWND の下に
  隠れる問題（T-010）は GenPreview では踏んでいない。プローブも「IME は確かめていない」（`hwnd-child-window.md:218-219`）

### 3.3 マウス・ホイール・クリックでのウィンドウ選択

| 操作 | 実装 | 出典 |
|---|---|---|
| クリック | ビューア自身が受ける（選択の開始、`SetCapture`）。**同時に `Event::Clicked` を積み**、ポーリングで `select-window`（別フレームなら `select-frame-set-input-focus` してから）。最大 100 ms 遅れる | `app.rs:855-874`、`genpreview.el:1044, 1062-1068` |
| クリックを親に転送して Emacs に選ばせる案 | 設計案で「未検証」のまま。実装していない | `GP/docs/emacs-embedded-view.md:212-213` |
| ドラッグ | `SetCapture` でビューアの外まで受けられる（プローブで確認）。外に出たら自動スクロール | `app.rs:875-899` |
| `Ctrl`+クリック | `GetKeyState` で読む（入力キューの連結で Emacs 宛ての状態が見える）→ `Event::Link` | `window.rs:405, 484`、`app.rs:858-861` |
| ホイール | `MouseWheelRouting` = 2（Windows の既定）なら**ビューアに直接届く**（`WM_MOUSEWHEEL`）。Emacs の `window-start` は動かない | `window.rs:450-481` |
| ホイールが Emacs に来た場合 | `mwheel-scroll` と `pixel-scroll-precision` を remap して module に転送（3 行ずつ） | `genpreview.el:1112-1117, 1153-1154` |
| カーソルの形 | `WM_SETCURSOR` でリンク・テキスト・それ以外を出し分け | `window.rs:491-501` |

**EmbTerm への示唆**: クリックで `Clicked` を出す形はそのまま使える。ただし端末では**マウスの報告（vim / yazi）**と
**選択**の切り替えが加わる（T-038）。ホイールも「スクロールバックを動かす」か「アプリに送る」かを状態で切り替える。

---

## 4. Emacs のウィンドウとの対応

### 4.1 位置合わせとフック

| 項目 | 実装 | 出典 |
|---|---|---|
| フック | `window-size-change-functions`・`window-buffer-change-functions` に加えて **`window-selection-change-functions`**（ビューアを選択中のウィンドウへ動かすため。プローブの 2 つに対する追加） | `genpreview.el:947-955` |
| 置き方 | `window-inside-pixel-edges` をそのまま親のクライアント座標として `SetWindowPos(…, SWP_NOZORDER \| SWP_NOACTIVATE)` | `genpreview.el:989-1006`、`embed.rs:441-446` |
| 重複を避ける | 最後に置いた `(WINDOW . EDGES)` を覚え、同じなら呼ばない | `genpreview.el:886, 1003` |
| 表示のタイミング | 読み込みが済むまでは置くだけで見せない（`SWP_SHOWWINDOW` は ready のときだけ）。済んだら `SW_SHOWNA` | `embed.rs:427-431, 444` |
| 重なり順 | 作成時と付け替え時に `HWND_BOTTOM`、以後は `SWP_NOZORDER`。**二度と上げない**（子フレームが隠れ、Emacs から取り返せない） | `window.rs:265`、`embed.rs:453-457` |

### 4.2 同じバッファを複数のウィンドウ・複数のフレームに出したとき（T-063 の材料）

**GenPreview の決定: ビューアは 1 枚だけ**（`GP/docs/emacs-embedded-view.md:437-438`、ADR 0008「結果」）。

| 状況 | 振る舞い | 出典 |
|---|---|---|
| split（同じバッファが 2 つのウィンドウ） | 出すウィンドウは**選択中 → 前に出していたウィンドウ → 最初のウィンドウ**の順で 1 つ選ぶ。**ほかのウィンドウには下のバッファ（文書の全文のテキスト）がそのまま見える** | `genpreview.el:982-987` |
| `other-window` で切り替え | `window-selection-change-functions` で、選んだほうへビューアが移る（同じ HWND を動かすだけ。スクロール位置は 1 つ） | テスト `emacs-view-test.el:218-226` |
| 複数フレーム | `get-buffer-window-list … 'visible` で全フレームから同じ規則で選び、フレームが変わったら **`SetParent` で付け替え**て `HWND_BOTTOM` に置き直す | `genpreview.el:996-1001`、`embed.rs:453-457` |
| ビューアを出したままそのフレームを消す | ビューアは親と一緒に破棄され、スレッドも終わる。ポーリングが `:alive` nil を見て、まだ表示しているウィンドウがあれば**作り直す（文書を読み直し、スクロール位置は先頭に戻る）** | `genpreview.el:1025-1031`、`GP/docs/emacs-embedded-view.md:413` |

**何が問題になり、どう扱ったか**: 設計案では段階 C の課題だったが、**作り込みはせず「1 枚だけ」で決着した**。
GenPreview は「下のテキストが見えれば十分」な用途なので、これで困らなかった。

**EmbTerm（T-016: split ではウィンドウごとに別のスクロール位置）への示唆**

- **GenPreview には前例が無い。** 流用できるのは「どのウィンドウに出すかを Lisp が決め、module には矩形を渡す」形と、
  `SetParent` による付け替えまで
- 候補 A（1 つの HWND を複数のペインに分ける）: 上下の split ならペインの間に**モードライン**、左右なら**縦の境界線**が挟まる。
  1 枚の矩形の HWND で覆うとそれを隠す（推測）。避けるには `SetWindowRgn` で穴を開けるか、結局ウィンドウごとに HWND を分けることになる
- 候補 B（ウィンドウごとに HWND）: GenPreview の「1 スレッド 1 `App`」をやめ、**1 本の UI スレッドが同じ端末の HWND を複数持つ**形が素直
  （同じスレッドなら端末の状態とデバイスを分け合いやすく、入力キューの連結も増えない。推測）。Lisp 側は
  `get-buffer-window-list` の各ウィンドウに `(端末, ウィンドウ) → HWND` を割り当て、いなくなったウィンドウの HWND を消す
- **端末の状態（VT・スクロールバック・PTY）は HWND とスレッドの寿命から切り離す。** GenPreview はフレームを消すと読み直しで復旧
  できたが、端末は作り直せない（プロセスが生きている）。HWND はいつ親ごと消えても良い「表示の器」にする
- 複数フレームは要件で「きれいでなくてよい」。GenPreview の「1 つだけに出して `SetParent` で付け替え」が最小の実装として使える

### 4.3 バッファが隠れたとき・タブ・子フレーム・テーマ・DPI

| 項目 | 振る舞い | 出典 |
|---|---|---|
| バッファが見えなくなった | 出すウィンドウが無ければ `view-hide`（`SW_HIDE`）。戻れば置き直して出す | `genpreview.el:991-995`、テスト結果 `GP/docs/emacs-embedded-view.md:284` |
| タブ（tab-bar） | 新しいタブを開くと元のウィンドウは `window-live-p` が nil になり隠れる。戻ると**同じウィンドウオブジェクトが生き返る**ので同じ位置に出る。tab-bar の高さは `window-inside-pixel-edges` に入っている | プローブ「実際の設定との組み合わせ」 |
| 最小化・モニタの移動 | 何もしなくてよい（子ウィンドウなので親と一緒に動く） | プローブ |
| 子フレーム（corfu） | ビューアの上に出る。ビューアを動かしても上のまま | `GP/docs/emacs-embedded-view.md:288` |
| **Emacs のテーマ（`load-theme`）** | **追従しない。** ビューアの色は GenPreview 自身のテーマ（`config.toml`、または `"auto"` で OS のライト / ダーク）。`frame-parameter 'background-mode` を見るのは Native 表示だけ | `genpreview.el:421-425` |
| OS のライト / ダーク | `"auto"` のときだけ追従。§1.3 の #6 の見えないトップレベルのウィンドウで受ける。切り替えの読み直しは裏で | `window.rs:270-317`、`app.rs:825-845` |
| DPI | Emacs は system-aware。150% のモニタでは Emacs ごと引き伸ばされ、**ぼやけるのは受け入れた**。`GetDpiForWindow` は 96 を返す。system-aware のスレッドで `GetDpiForMonitor` も仮想化された 96 を返す | プローブ「DPI」、`GP/docs/emacs-embedded-view.md:41` |

**EmbTerm への示唆**: 要件 §5.9 は「Emacs のテーマ（face）に追従するかどうか」を設定に挙げている。GenPreview にその前例は無いので、
`enable-theme-functions`（Emacs 29 以降）などで face の色を読み、module に色を送る形を自前で作る（推測。T-034）。

---

## 5. T-026: `emacs` クレートの版と module の作法

### 5.1 版と feature

| 項目 | 値 | 出典 |
|---|---|---|
| クレート | `emacs = { version = "0.23", features = ["emacs-28"] }`（`Cargo.lock` では `emacs 0.23.0`、`emacs_module 0.23.0`、`emacs-macros 0.22.0`） | `GP/Cargo.toml` の `[workspace.dependencies]` |
| `emacs-28` が要る理由 | ユニバイト文字列（`&[u8]`/`Vec<u8>` の `IntoLisp`）と、Emacs 27 以前の GC 不具合の回避策を外すため（`open_channel` は使わなくなったが feature は残す） | ADR 0002、ADR 0006「帰結」 |
| ほかの feature | `emacs-32-experimental`（`canvas` 型。`types/mod.rs:4-14`）があるが使っていない | クレートの `Cargo.toml` |
| クレートの MSRV | 1.87（GenPreview / EmbTerm の `rust-version` は 1.95） | クレートの `Cargo.toml` |
| Emacs | GNU 公式の Windows ビルド 31.1（`x86_64-w64-mingw32`）。**MSVC でビルドした DLL を mingw の Emacs から読んで問題なし** | ADR 0002 |
| ターゲットと CRT | `x86_64-pc-windows-msvc`、`-C target-feature=+crt-static`（`.cargo/config.toml`）。**EmbTerm の `.cargo/config.toml` も同じ設定になっている** | `GP/.cargo/config.toml` |
| クレートの種類 | `[lib] name = "genpreview_module"`、`crate-type = ["cdylib"]` | `GP/apps/genpreview-emacs/Cargo.toml` |

**EmbTerm の `Cargo.toml` はすでに `emacs = { version = "0.23", features = ["emacs-28"] }` で GenPreview と同じ。そのまま流用してよい。**
Emacs 31.1 の module API との関係は、GenPreview が 31.1 で動いていること以上は確かめていない（`emacs-module.h` の差分は未調査）。

### 5.2 module の作法（`GP/apps/genpreview-emacs/src/lib.rs`）

| 作法 | 書き方 | 出典 |
|---|---|---|
| GPL 互換の宣言 | `emacs::plugin_is_GPL_compatible!();` | `lib.rs:18` |
| 初期化と名前 | `#[emacs::module(name = "genpreview-module", defun_prefix = "genpreview-module", separator = "-")] fn init(_: &Env) -> Result<()>` | `lib.rs:20-23` |
| 関数の登録 | `#[defun]` を付けるだけ。Rust の `view_create` が Lisp の `genpreview-module-view-create` になる（`_` → `-`） | `lib.rs:52-58` ほか |
| 引数 | `String`・`i64`・`f64`・`Option<String>`（nil を受ける）・`emacs::Vector`・`Value`。整数と小数の両方を受けるなら `Value` で受けて自前で変換（`num`） | `lib.rs:34-40, 337-352` |
| 戻り値 | `Result<T>`。plist は `env.intern(":alive")` と値を並べて `env.list(&items)`。シンボルは `env.intern`、cons は `env.cons` | `lib.rs:369-399, 410-424` |
| user-ptr | `#[defun(user_ptr)] fn view_create(…) -> Result<RefCell<EmbeddedView>>`。受ける側は `&RefCell<EmbeddedView>`。**GC で回収されると `Drop` が走る**（`finalize<T>` が `Box::from_raw` して落とす。`user_ptr.rs:75-81`）。`EmbeddedView::drop` は `Close` を投げるだけで待たない | `lib.rs:299-304`、`embed.rs:254-258` |
| 明示的な破棄 | GC 任せにせず、`kill-buffer-hook` → `view-destroy` で閉じる | `genpreview.el:263-264, 941-945` |
| エラー | `emacs::Error::msg(String)` を返すと Lisp のエラーになる（`lisp_err`）。Lisp 側は `condition-case` で `message` に落とす | `lib.rs:30-32`、`genpreview.el:976-978, 1016-1019` |
| スレッドローカルな資源 | Lisp スレッドだけで使う描画資源は `thread_local! RefCell<Option<Renderer>>` | `lib.rs:25-28` |
| 待たない API | `view-*` は `view-selected-text` を除いて「積んで投げる」か「写す」だけ | `lib.rs:285-289` |

---

## 6. T-030: 読み込み済み DLL の更新の手順

**Emacs は module をアンロードできず、読み込み中の DLL はロックされる。** GenPreview は「コピーしてから読む」で回避している。

| 手順 | 内容 | 出典 |
|---|---|---|
| 1. 版付きの名前でコピー | 元の DLL の**更新時刻とサイズ**から `genpreview_module-YYYYmmddHHMMSS-SIZE.dll` を作り、`temporary-file-directory` にコピーしてから `module-load` | `genpreview.el:153-174` |
| 2. 古いコピーの掃除 | 同じパターンの古いコピーを `delete-file`（読み込み中のものは消せないので `ignore-errors`） | `genpreview.el:166-169` |
| 3. 元の場所を module に教える | コピー先では同梱の DLL（`heif.dll`、`pdfium.dll`）が見つからないので、`genpreview-module-setup` に**元のディレクトリ**を渡す。module は `LoadLibraryExW(フルパス, LOAD_WITH_ALTERED_SEARCH_PATH)` で先に読む。**このパスは `\` 区切りでないと働かない**（Emacs は `/` で渡す）。プロセス全体の `SetDefaultDllDirectories` は変えない | `lib.rs:51-58`、`heic.rs:52-74`、`GP/docs/architecture.md:941-948` |
| 4. 新しい版を使う | **Emacs の再起動が要る**（同じ Emacs で 2 回目の `module-load` はしない。`genpreview--module-version` で 1 回だけ） | ADR 0008「結果」、`genpreview.el:157` |
| 配布物 | `cargo xtask dist` が release ビルドして `target/dist/genpreview/` に exe・module・`genpreview.el`・同梱 DLL・ライセンスを集める。**`genpreview.el` は自分の隣の module を探す**（`genpreview-module-file` の既定） | `GP/xtask/src/main.rs:80-110`、`genpreview.el:53-58` |

**EmbTerm への示唆**: 同じ手順がそのまま使える。`conpty.dll` + `OpenConsole.exe` を同梱するなら（T-007）、
**元のディレクトリを module に知らせてフルパスで読む**ところまで同じ事情になる（`OpenConsole.exe` は `conpty.dll` が自分の隣から
起動するので、`conpty.dll` は元の場所のものを `LoadLibraryExW` で読む必要がある。推測）。
開発中は「使っている Emacs には読み込まず、別の `emacs -Q` で試す」（プローブ「測り方」）も踏襲する。

---

## 7. T-031: panic の境界

| 境界 | 扱い | 出典 |
|---|---|---|
| Lisp から呼ばれる関数（`#[defun]`） | **`emacs` クレートが `catch_unwind` で包み、Lisp のエラー `rust-panic` に変える**（`func.rs:213-219`、`error.rs:393-438`）。Phase 0 で Emacs が落ちないことを確認 | ADR 0002 #5 |
| module の初期化 | 同じくクレートが `catch_unwind`（`init.rs:109`） | クレートのソース |
| `wndproc` | **自前で `catch_unwind(AssertUnwindSafe(…))`**。panic したらトレースに書いて `DefWindowProcW` に任せる（`extern "system"` の外へ unwind させない） | `window.rs:383-397` |
| ワーカースレッド | panic してもそのスレッドが終わるだけ。`join()` の `Err` を `Event::Error("… panic しました")` にする | `embed.rs:394-401` |
| `release` プロファイル | **`panic = "abort"` にしない**（unwind が要る）。EmbTerm の `Cargo.toml` も同じコメント付きで設定済み | `GP/Cargo.toml`、`GP/docs/architecture.md:927-928` |
| 防げないもの | ネイティブのライブラリ（pdfium）のアクセス違反は Emacs ごと落ちる。別プロセス（`--serve`）に逃がす構想はあるが、起きなかったので作っていない | `GP/docs/architecture.md:573-577, 984` |

**穴（推測を含む）と EmbTerm での注意**

- **user-ptr の finalizer は `catch_unwind` されていない**（`user_ptr.rs:75-81`）。`Drop` で panic すると `extern "C"` の境界で
  プロセスが abort する。`EmbeddedView::drop` は `Mutex::lock().unwrap()` を呼ぶ（`embed.rs:197`）ので、毒された `Mutex` なら落ちうる。
  **EmbTerm では `Drop` の中で `unwrap` しない**（`lock()` の `Err` は `into_inner` で拾う）
- ビューアのスレッドのメッセージループそのもの（`apply`・`finish_loading`、`embed.rs:356-379`）は `catch_unwind` していない。
  panic するとスレッドが終わり、`alive = false` を書く後片付け（`embed.rs:386-389`）も飛ぶ（推測）。EmbTerm では
  **スレッドの本体も `catch_unwind` で包み、終わったことを必ず共有状態に書く**
- libghostty-vt（C ABI、Zig）の中で落ちた場合は panic ではないので止められない。pdfium と同じ位置づけ（T-005 の評価項目）
- `Shared` のロックは `lock().unwrap()`。Lisp 側の関数で毒に当たっても `rust-panic` になるだけで Emacs は落ちない

---

## 8. EmbTerm で流用できる部品（T-015 の材料）

### 8.1 部品の一覧

| 部品 | 場所 | 行数 | 依存 | 流用のしかた |
|---|---|---|---|---|
| **埋め込みの最小形（自己完結）** | `GP/spikes/embed-surface/src/lib.rs`（+ `run.el` 178 行） | 435 | `emacs`、`gp-render`（`Gpu`/`Surface` だけ） | **最初の雛形に最適。** スレッド・子 HWND・`PostMessage`・付け替え・3 方式の描画を 1 ファイルで持つ。T-028 の出発点 |
| 埋め込みの本体 | `GP/crates/window/src/embed.rs` | 569 | `gp-core`（`Command`・`Config`）、`gp-render`、`App` | 骨組み（`Shared`/`Request`/`EmbeddedView`/`spawn`/メッセージループ/`apply`）を**コピーして作り替える**。`Utf16Map`（約 50 行）は T-017 で端末のテキストをバッファに写すなら使える |
| 子ウィンドウの作成・設定変更の受け | `GP/crates/window/src/window.rs:227-317` | 約 90 | `windows` | `create_child_window`、`create_setting_listener` はほぼそのまま |
| `wndproc` の作法 | `window.rs:375-398` | 約 25 | — | `MA_NOACTIVATE` を先に返す、`try_borrow_mut` で再入を避ける、`catch_unwind` |
| `Host` の分け方 | `GP/crates/window/src/app.rs:32-49`、表は `GP/docs/emacs-embedded-view.md:85-98` | — | — | 単体の exe（要件 §5.11）と module で `App` を共有するときの設計の型 |
| GPU | `GP/crates/render/src/gpu.rs` | 64 | `windows` | そのまま |
| スワップチェーン | `GP/crates/render/src/surface.rs` | 102 | `windows` | そのまま（`Present` の同期の設定だけ T-008 で見直す） |
| ブラシのキャッシュ | `GP/crates/render/src/brushes.rs` | 29 | `gp-core::Color` | `Color` を置き換えて使う |
| IME・クリップボード | `GP/crates/window/src/system.rs` | 109 | `windows` | `set_clipboard_text`/`get_clipboard_text`/`enable_ime`/`set_ime_position` |
| トレース | `GP/crates/window/src/trace.rs` | 19 | — | 環境変数で有効になる計測ログ。そのまま |
| module の橋渡し | `GP/apps/genpreview-emacs/src/lib.rs:285-437` | 約 150 | `emacs` | `view-*` 関数の形（user-ptr、plist、イベントのリスト、タイムアウト付きの同期） |
| Lisp: 埋め込み | `GP/lisp/genpreview.el:870-1189` | 約 320 | — | 位置合わせ（フック 3 つ、置き直しの省略、付け替え）、ポーリング、作り直し、クリック → `select-window`、`emulation-mode-map-alists` |
| Lisp: module の読み込み | `GP/lisp/genpreview.el:153-174` | 22 | — | 版付きのコピー（T-030） |
| 自動テストの型 | `GP/tools/emacs-view-test*.el`、`hwnd-probe.dll` の `pixel`/`click-at`/`wheel-at` | — | — | `emacs -Q` を GUI で起こして自動で進め、画面の色と本物の入力で確かめる方式 |
| xtask | `GP/xtask/src/main.rs` | 110 | — | `dist` の形だけ（同梱物は違う） |

### 8.2 流用するときの注意

1. **`gp-window`・`gp-render` を path 依存にするのは勧めない。** `App` は文書のモデル（`View::Flow`/`View::Paged`、`gp_core::Document`、
   `Registry`、`notify` の監視、検索、ズーム）と一体で、`gp-render` も `gp-core`・`gp-format-svg` に依存する。端末が欲しいのは
   その下の薄い層（デバイス・スワップチェーン・子 HWND・スレッドの作法）だけで、合計 1,000 行程度
2. **共通クレートに切り出す案**（例: `win-embed` に `Gpu`・`Surface`・子 HWND・`Shared`/`Request` の型を置き、GenPreview と EmbTerm の両方が
   使う）は、GenPreview 側の改修が要る。両者が固まるまでは保留が妥当（推測）。まず**コピーして EmbTerm の中で育て**、
   落ち着いてから切り出すかを決める
3. **コピーしたら出典を残す**（ファイルの先頭に `GenPreview <commit> の <path> から` と書く）。GenPreview 側で直った不具合
   （§1.3 の #1〜#3 のような `WM_SIZE` の取りこぼし）を追いかけやすくする
4. ライセンスは GenPreview も EmbTerm も `MIT OR Apache-2.0`、作者も同じなのでコピーに支障はない
5. **そのまま持ち込まないほうがよいもの**
   - `PostThreadMessageW`（§2.3。HWND 宛てにする）
   - ビューアごとの `Gpu::create`（§2.2。端末が多いなら共有）
   - 1 スレッド 1 `App` の `thread_local!`（T-063 で 1 スレッドが複数の HWND を持つなら変える）
   - `WS_VSCROLL`/`WS_HSCROLL`（端末のスクロールバーを OS のものにするかは別に決める）
   - 「HWND が消えたら読み直して作り直す」復旧（端末は状態を作り直せない。§4.2）

---

## 9. EmbTerm の決定への示唆（まとめ）

| 決定 | 示唆 |
|---|---|
| T-006 スレッド | GenPreview の形（Lisp → 積んで `PostMessage`、module → `Mutex` の共有状態をポーリング、唯一の同期はタイムアウト付き）を踏襲する。そのうえで**端末ごとに PTY 読み・VT 解釈のスレッドを UI スレッドから分け**、UI スレッドは描くだけ。**端末の状態は HWND と UI スレッドより長生きにする** |
| T-008 描画 | **Direct2D + フリップモデルで黒くならないことは確かめ済み。** 未確認なのは連続描画の負荷と `Present(1)` の待ち。D3D/D2D デバイスはプロセスで 1 つを共有し、スレッドごとにデバイスコンテキストを作る（ワーカーで実績あり） |
| T-009 キー | 案 A（フォーカスを Emacs に残す）は、子 HWND 側の作法（`WS_EX_NOACTIVATE`・`MA_NOACTIVATE`・クリックで `select-window`）とキーマップ側の作法（`emulation-mode-map-alists` の先頭を取り返し続ける）が揃っている。**IME（T-010）は GenPreview に前例が無い** |
| T-015 共有 | 薄い層（約 1,000 行）を**コピーして作り替える**。path 依存は `gp-core` ごと引き込むので避ける。共通クレート化は後で判断 |
| T-063 split | GenPreview は「1 枚だけ」で逃げた。**ウィンドウごとのスクロールには HWND を複数にする（候補 B）が素直**で、1 枚の HWND で覆う候補 A はモードラインと境界線を隠す問題がある（推測）。複数フレームは GenPreview の「1 つだけに出して `SetParent`」で足りる |

---

## 10. 確かめていないこと（GenPreview でも未検証）

| 項目 | 状態 |
|---|---|
| 毎秒数十回の描き直しでの UI スレッドの占有と、クリックが重なったときの Emacs のキーの遅れ | 未測定（GenPreview は描き直しが少ない） |
| `Present(1)` の待ちが入力キューの連結を通じて Emacs に響くか | 未測定（推測で懸念） |
| 複数のビューアで 1 つの D3D/D2D デバイスを共有する形 | 実装していない |
| 子 HWND を出したまま IME（tr-ime）で変換したときの、変換中の文字列と候補ウィンドウの位置 | 未検証（プローブ、GenPreview とも） |
| クリックを親に転送して Emacs 自身にウィンドウを選ばせる案（100 ms の遅れを無くす） | 未検証 |
| 1 本の UI スレッドが同じ親の子 HWND を複数持つ形 | 実装していない |
| モーダルループ中の `PostThreadMessageW` の取りこぼし | 未観測（Win32 の既知の挙動からの推測） |
| Emacs 31.1 の `emacs-module.h` と `emacs` クレート 0.23 の対応の詳細 | 未調査（GenPreview が 31.1 で動いていることのみ） |
