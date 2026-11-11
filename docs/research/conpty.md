# ConPTY の調査: VT の素通し・キー入力・yazi の画像

- 日付: 2026-11-11
- **TODO T-021 / T-022 / T-023 の記録**
- 調べ方: microsoft/terminal の main のソース（`src/host`、`src/terminal`、`src/winconpty`）・リリースノート・issue / PR、
  yazi v26.9.1 のソース、ghostel のソースとサイト、他の端末（wintty・cleat など）の報告。
  **手元で ConPTY の各版を実測した**（付録 A。スクリプトは [conpty-probe/](conpty-probe/)）
- 実測の環境: Windows 11 Pro 26H2 ビルド 26300.9711（`conhost.exe` 10.0.26100.9949）

**凡例**: 「実測」は付録 A の方法で 2026-11-11 に確かめたこと。「ソース」は microsoft/terminal の main（2026-11 時点）を読んだこと。
「推測」「不明」はそのとおり。

---

## 0. 要点

| | 結論 |
|---|---|
| **OS 同梱の ConPTY** | **ビルド 26300 でも旧方式（VtEngine が画面を作り直して送る）だった（実測）。** APC（Kitty graphics）と DCS（Sixel）は**捨てられ**、OSC 52/9/777/7 と DEC 2026 は**位置がずれて**届く。DA1・DA2・DECRQM・DECRQSS には **conhost が自分で答える**ので、アプリは端末の能力を知れない |
| **再配布版 1.22 以降** | **アプリが書いた VT をそのまま端末に流す**（conhost も同じ列を解釈して自分のバッファを保つ）。OSC 8/52/9/777/7/1337、DEC 2026、APC、DCS、KKP の push/pop がすべて**元の位置のまま**届き、**問い合わせは端末が答え、答えはアプリに届く**（実測: 1.22 / 1.24 / 1.25 / 1.26） |
| **passthrough モード** | **フラグとしてはもう無い。** WT 1.14 の実験的な `PSEUDOCONSOLE_PASSTHROUGH_MODE`（0x8）は 2024-03 に削除。1.22 の作り直しで「素通し」が既定になった。0x8 は今は `PSEUDOCONSOLE_GLYPH_WIDTH_GRAPHEMES` の値 |
| **キー** | ConPTY は起動時に必ず `CSI ?9001h`（win32-input-mode）と `CSI ?1004h` を端末に求める。**端末は win32-input-mode でキーを送るのが基本。** アプリが KKP を有効にしたら（`CSI > flags u` は素通しで端末に届く）KKP で送る（Windows Terminal と同じ方針） |
| **文字幅** | 再配布版の既定は**書記素クラスタ単位・eaw は 1 桁**。**eaw を 2 桁にするフラグは 1.25 から**（`PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE` = 0x20）。そのときも**罫線 `─` は 1 桁のまま**（実測） |
| **Kitty graphics** | **再配布版（1.22 以降、できれば 1.25 以降）なら通る**（実測）。OS 同梱では通らない。yazi は EmbTerm が `CSI 16 t` と KGP の問い合わせに答えれば KGP を使える |

---

## 1. 前提: ConPTY の版と系譜

### 1.1 版ごとの大きな変化

| 時期 | 版 | 変化 | 出典 |
|---|---|---|---|
| 2022 | WT 1.14 | 実験的な passthrough モード（`experimental.connection.passthroughMode`、`PSEUDOCONSOLE_PASSTHROUGH_MODE` = 0x8）。「cmd と WSL ではおおむね動く、PowerShell はだめ」 | [v1.14 リリースノート](https://newreleases.io/project/github/microsoft/terminal/release/v1.14.1432.0)、[profile-advanced](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/profile-advanced) |
| 2024-03 | （1.21 系の前） | **passthrough モードを削除**（#16954 "Remove VtApiRoutines"）。理由は「性能・リサイズ後のずれなど他の問題を解決しない」。同時に `PSEUDOCONSOLE_WIN32_INPUT_MODE`（0x4）も定義から消えた | [#16954](https://github.com/microsoft/terminal/pull/16954)、`src/inc/conpty-static.h` の v1.17 / v1.21 の比較 |
| 2024-08 プレビュー / 2025-02 安定 | **WT 1.22** | **VtEngine を廃止**（#17510 "Goodbye VtEngine Edition"）。コンソール API の呼び出しはその場で VT に直し、**アプリが出した VT は手を加えずに端末へ渡す**。Sixel（conhost も描く）、書記素クラスタ、DCS の素通し | [#17510](https://github.com/microsoft/terminal/pull/17510)、[v1.22.2362.0](https://github.com/microsoft/terminal/releases/tag/v1.22.2362.0)、[#1173（2024-08-01 に #17510 で close）](https://github.com/microsoft/terminal/issues/1173) |
| 2026-02〜03 プレビュー / 2026-10-02 安定 | **WT 1.25** | WT に Kitty keyboard protocol（#19817）、同期出力、**eaw の幅の設定**（#19864、ConPTY へは `--ambiguousIsWide`）。ConPTY はリサイズ後に CPR で位置を取り直す（#19535）、**未知のシーケンスの後に CPR で位置を取り直す**（#20009）、DA1 の応答を待たない（#20536）、CPR を再送しない（#20510） | [v1.25.622.0](https://github.com/microsoft/terminal/releases/tag/v1.25.622.0)、[devblog](https://devblogs.microsoft.com/commandline/windows-terminal-preview-1-25-release/)、[v1.26.2734.0](https://github.com/microsoft/terminal/releases/tag/v1.26.2734.0)、[#19926](https://github.com/microsoft/terminal/issues/19926) |
| 2026-10〜11 | WT 1.26 プレビュー | OSC 777 の通知（WT 側）、KKP の修正。**1.25.3131 / 1.26.3133 が 2026-11-11 に出ている** | [releases](https://github.com/microsoft/terminal/releases) |
| 2026-11 時点 | WT（未マージ） | Kitty graphics の PR が 3 本。#20885 は close、#20872 はレビュー中（「方向は正しいが磨きが要る」） | [#20872](https://github.com/microsoft/terminal/pull/20872)、[#20885](https://github.com/microsoft/terminal/pull/20885) |

**OS 同梱の ConPTY がいつ新方式になるかは公式の記述を見つけられなかった。** ビルド 26300 では旧方式（実測、§2.2）。
他の端末の報告も「OS 同梱は APC と Sixel を落とす」（[cleat #235](https://github.com/flotilla-org/cleat/pull/235)、ビルド 26200、2026-09）で一致する。
[wintty #263](https://github.com/deblasis/wintty/issues/263) は「24H2 以降の OS 同梱は #17510 入り」と書くが、**手元の実測と合わない**（OSC の並べ替え・OSC 8 の書き換えという旧方式の症状が出る）。

### 1.2 手元にある版

| 場所 | 版 | 備考 |
|---|---|---|
| OS 同梱（`kernel32!CreatePseudoConsole` → `conhost.exe`） | 10.0.26100.9949 | 旧方式 |
| `~/.emacs.d/ghostel/conpty.dll` + `x64/OpenConsole.exe` | **1.24.2605.12001**（NuGet 1.24.260512001 相当）。Microsoft の署名あり | ghostel 0.62.0 の同梱物。`--ambiguousIsWide` を持たない |
| WezTerm nightly（`scoop/apps/wezterm-nightly`） | 1.22.2502.04002 | WezTerm は `--inheritcursor` で起動している（プロセスのコマンドラインで確認） |
| NuGet `Microsoft.Windows.Console.ConPTY` | 1.24.260303001 〜 1.24.261001001、**1.25.260930003（最新の安定）**、1.26.260930004-preview | [nuget.org](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY)。MIT。1.25 と 1.26 を落として実測した |

### 1.3 再配布版の読み込み方（ソース）

`conpty.dll` は自分と同じディレクトリの `OpenConsole.exe` を探し、無ければ **`x64\` / `arm64\`（実行中の CPU の名前）の下**を探す。
**どちらにも無いと黙って `System32\conhost.exe`（OS 同梱の旧方式）を使う**（`src/winconpty/winconpty.cpp` の `_ConsoleHostPath`）。
ghostel の配置（`conpty.dll` と `x64/OpenConsole.exe`）はこの規則に合っている。NuGet の中身は
`runtimes/win-x64/native/conpty.dll` と `build/native/runtimes/x64/OpenConsole.exe` なので、並べ直す必要がある。

`conpty.dll` は `OpenConsole.exe --headless [--inheritcursor] [--ambiguousIsWide] [--textMeasurement …] --width --height --signal --server` で起動する。
`CreatePseudoConsole` のフラグ（`src/inc/conpty-static.h`、main）:

| フラグ | 値 | 意味 | 使える版 |
|---|---|---|---|
| `PSEUDOCONSOLE_INHERIT_CURSOR` | 0x1 | 起動時に端末へ CPR を送り、今のカーソル位置を引き継ぐ | 古くから。OS 同梱にもある |
| `PSEUDOCONSOLE_GLYPH_WIDTH_GRAPHEMES` | 0x08 | 書記素クラスタで幅を数える | 1.22 以降（再配布版） |
| `PSEUDOCONSOLE_GLYPH_WIDTH_WCSWIDTH` | 0x10 | `wcswidth` 風 | 同上 |
| `PSEUDOCONSOLE_GLYPH_WIDTH_CONSOLE` | 0x18 | 旧 conhost と同じ | 同上 |
| `PSEUDOCONSOLE_AMBIGUOUS_IS_WIDE` | 0x20 | eaw を 2 桁 | **1.25 以降**（1.24 の `conpty.dll` は無視する。実測） |
| ~~`PSEUDOCONSOLE_RESIZE_QUIRK`~~ / ~~`WIN32_INPUT_MODE`~~ / ~~`PASSTHROUGH_MODE`~~ | 0x2 / 0x4 / 0x8 | 旧い実験的なフラグ。**0x8 は今は別の意味** | 使わない |

ほかに `ConptyReparentPseudoConsole`（擬似コンソールのウィンドウの親を変える）、`ConptyShowHidePseudoConsole`、
`ConptyClearPseudoConsole`、`ConptyReleasePseudoConsole`（24H2 で OS にも入った API。子が終わったら conhost も終わる）がある
（`conpty.dll` のエクスポートを `strings` で確認）。

---

## 2. T-021: ConPTY は VT をどう扱うか

### 2.1 分かったこと: 仕組み（ソース）

**1.22 以降（再配布版）**: `WriteCharsVT`（`src/host/_stream.cpp`）は、アプリが書いた文字列を
conhost 自身のパーサに通して自分の画面バッファを更新し、**同じ文字列をそのまま端末に書く**。手を加えるのは次だけ。

- `DISABLE_NEWLINE_AUTO_RETURN` が立っていなければ LF を CRLF にする
- RIS・DECRST 1004・DECRST 9001 の直後に `CSI ?1004h` / `CSI ?9001h` を差し込む（ConPTY が頼るモードを戻すため）
- conhost の応答は捨てる。`ConhostInternalGetSet::ReturnResponse` に
  **「ConPTY should not respond to requests. That's the job of the terminal.」**（`src/host/outputStream.cpp`）
- KKP の `CSI = u` / `CSI > u` / `CSI < u` / `CSI ? u` は ConPTY では conhost が処理しない（`adaptDispatch.cpp` の `if (_api.IsConPTY()) return;`）。端末に素通しされる
- コンソール API（`WriteConsoleOutput`・`SetConsoleCursorPosition`・`ScrollConsoleScreenBuffer` など）は**その場で VT に直す**。
  `ScrollConsoleScreenBuffer` は、端末の DA1 に 28（矩形操作）があれば DECCRA / DECFRA を使う（`src/host/getset.cpp`）

**OS 同梱（旧方式）**: conhost が解釈した結果を VtEngine が「画面の差分」として描き直して送る。
知らないシーケンスは捨てるか、描画のタイミングでまとめて吐くので位置がずれる（#1173 の長年の課題）。

### 2.2 実測: 何が届くか

子プロセスが `ENABLE_VIRTUAL_TERMINAL_PROCESSING` で `<名前>` + シーケンス + `</名前>` を書き、ホストが受け取った列を比べた（付録 A）。

| シーケンス | OS 同梱（26300） | 再配布版 1.22 / 1.24 / 1.25 / 1.26 |
|---|---|---|
| OSC 8 ハイパーリンク | 届くが**書き換わる**（`ESC]8;id=24684-1;URL` と id が付く） | **そのまま** |
| OSC 52 クリップボード | 届くが**位置がずれる**（出力の先頭にまとめて出る） | そのまま |
| OSC 9 / 777 通知 | 届くが位置がずれる | そのまま |
| OSC 7 cwd | 届くが位置がずれる | そのまま |
| OSC 1337（iTerm2 画像） | 届くが位置がずれる | そのまま |
| DEC 2026 同期出力 | `?2026h` `?2026l` が**中身と切り離されて**出る（意味を失う） | そのまま |
| **APC（Kitty graphics）** | **捨てられる** | **そのまま** |
| **DCS（Sixel）** | **捨てられる** | **そのまま**（conhost も Sixel を解釈して自分のバッファに描き、カーソルを進める） |
| KKP push / pop（`CSI >1u` `CSI <u`） | 届くが位置がずれる | そのまま |
| DECSCUSR（カーソルの形） | 届くが位置がずれる | そのまま |
| 下線の種類・色（`CSI 4:3m` `CSI 58:2::r:g:bm`） | conhost が描き直す（順番が入れ替わる） | そのまま |

**1.25 / 1.26 では、conhost が知らないシーケンス（OSC 1337、APC）の直後に `CSI 6n` を端末に送り、返ってきた位置へ `CUP` を出す**（実測。#19926 の修正 #20009）。
知っているもの（OSC 8/52/9/777/7、DCS Sixel、KKP）の後には出ない。1.24 は出さないので、**APC でカーソルが動くと conhost とずれる**（#19926、1.23 で報告）。

### 2.3 実測: 問い合わせに誰が答えるか

子が VT 入力モード（`ENABLE_VIRTUAL_TERMINAL_INPUT`）で問い合わせを書き、ホスト（テスト用の端末）が答え、子が受け取ったものを記録した。

| 問い合わせ | OS 同梱（26300） | 再配布版 1.24 / 1.25 |
|---|---|---|
| DA1 `CSI c` | **conhost が答える**（`CSI ?61;6;7;21;22;23;24;28;32;42c`）。端末には届かない | 端末に届き、**端末の答えがアプリに届く** |
| DA2 `CSI >c` | conhost が答える（`CSI >0;10;1c`） | 端末が答える |
| XTVERSION `CSI >0q` | 端末に届き、端末の答えが届く | 同左 |
| DECRQM `CSI ?2026$p` | conhost が答える（**`?2026;0$y` = 知らない**） | 端末が答える |
| DECRQSS `DCS $q SP q ST` | conhost が答える（`DCS 0$r ST` = 無効） | 端末が答える |
| KKP の問い合わせ `CSI ?u` | 端末が答える | 端末が答える |
| Kitty graphics の問い合わせ（APC `a=q`） | **捨てられる（答えが返らない）** | 端末が答える |
| `CSI 16t`（セルのピクセル） | 端末が答える | 端末が答える |
| OSC 11 `?`（背景色） | **捨てられる** | 端末が答える |
| `CSI ?996n`（配色の明暗） | 端末が答える | 端末が答える |
| CPR `CSI 6n` | アプリが出したものは端末が答える | 同左。**加えて conhost 自身も出す**（INHERIT_CURSOR のとき起動時、1.25 以降はリサイズ後と未知のシーケンスの後） |

答えの列は ConPTY の入力側を通る。VT 入力モードのアプリには**そのまま**届く（`InputStateMachineEngine` が VT 入力のアプリには
知らない CSI をそのまま流す。ソース・実測）。ただし**起動時の最初の DA1 の答えは conhost が取る**（端末の能力を記録するため。§2.4）。

### 2.4 ConPTY が端末に送ってくるもの（実測・ソース）

| いつ | OS 同梱 | 再配布版 1.22〜1.26 | 端末がすること |
|---|---|---|---|
| 起動 | `CSI ?9001h CSI ?1004h`、画面の消去と全体の描き直し、OSC 0（タイトル = 子の exe のパス） | `CSI 1t CSI c CSI ?1004h CSI ?9001h` | **DA1 に答える**（conhost は 61 以上の適合レベルと属性を読み、28 があれば矩形操作を使う）。`CSI 1t`（XTWINOPS: 窓を戻す）は無視してよい |
| INHERIT_CURSOR | `CSI 6n` | `CSI 6n`（DA1 と合わせて最大 1 秒待つ） | CPR に答える |
| リサイズ後（1.25〜） | — | `CSI 6n` | CPR に**正しく**答える |
| 未知のシーケンスの後（1.25〜） | — | `CSI 6n` → 答えの位置へ `CUP` | 同上。**間違った位置を答えると画面が崩れる**（実測でわざと違う値を返すとカーソルが飛んだ） |
| コンソールアプリが `ENABLE_MOUSE_INPUT` を変えた | `CSI ?1003;1006h` / `l` | 同左 | SGR でマウスを送る |
| 終了 | `CSI ?9001l CSI ?1004l` | `CSI ?1004l CSI ?9001l` | |
| 窓の表示・非表示 | | `CSI 1t` / `CSI 2t` | 無視してよい |

### 2.5 passthrough モード

- **今は「passthrough を有効にするフラグ」は無い。** 1.22 以降は既定で「VT をそのまま流す + conhost も並行して解釈する」動き。
  conhost がバッファを持ち続けるので、コンソール API のアプリ（cmd・PSReadLine）と VT のアプリが混ざっても動く
- 旧い `PSEUDOCONSOLE_PASSTHROUGH_MODE`（0x8）は WT 1.14〜1.20 前後の実験で、2024-03 に削除（[#16954](https://github.com/microsoft/terminal/pull/16954)）。
  **今の `conpty.dll` に 0x8 を渡すと「書記素で幅を数える」の意味になる**
- **OS 同梱の ConPTY（ビルド 26300）には素通しが無い。** 素通しが欲しければ再配布版を使うしかない
- ConPTY を通さない方法（子に生のパイプを渡す「ConPTY bypass」）を試みる端末もある（[wintty #263](https://github.com/deblasis/wintty/issues/263)）が、
  コンソール API のアプリ（cmd・PowerShell 5.1・PSReadLine・nyagos・ネイティブ vim）が動かないので **EmbTerm の目標には合わない**

### 2.6 文字幅（T-011 / T-012）

**実測**（子が 1 文字書いた前後の `GetConsoleScreenBufferInfo` のカーソルの差 = conhost が数えた桁数）:

| 文字 | OS 同梱 | 再配布版 既定（1.22〜1.26） | `WCSWIDTH`（0x10） | `CONSOLE`（0x18） | `AMBIGUOUS_IS_WIDE`（0x20、1.25） |
|---|---|---|---|---|---|
| `①` U+2460（eaw） | 1 | **1** | 1 | 1 | **2** |
| `★` U+2605（eaw） | 1 | 1 | 1 | 1 | 2 |
| `※` U+203B（eaw） | 1 | 1 | 1 | 1 | 2 |
| `─` U+2500（eaw、罫線） | 1 | 1 | 1 | 1 | **1（2 にならない）** |
| `°` `α`（eaw） | 1 | 1 | 1 | 1 | 2 |
| `あ` | 2 | 2 | 2 | 2 | 2 |
| `😀` | 2 | 2 | 2 | 2 | 2 |
| `❤️`（U+2764 U+FE0F） | 2 | 2 | **1** | 2 | 2 |
| 家族の ZWJ 絵文字（3 人） | **8** | **2** | 6 | 8 | 2 |
| `é`（e + U+0301） | **2** | **1** | 1 | 2 | 1 |

- 再配布版の既定は**書記素クラスタ単位**（`CodepointWidthDetector` の `_mode = Graphemes`、Unicode 16 の表）、**eaw は 1 桁**（`_ambiguousWidth = 1`）。
  WT 1.25 の設定の既定も narrow（[#19864](https://github.com/microsoft/terminal/pull/19864)）
- eaw を 2 桁にしたときも `─` が 1 桁のままなのは、Windows Terminal の幅の表が罫線を狭い側に上書きしているためと思われる（**推測**。理由は未確認、結果は実測）
- OS 同梱は**結合文字・ZWJ をまとめない**ので、`é` や ZWJ 絵文字で conhost のカーソルと端末の描画がずれる
- #19864 の注意書き: 「PSReadLine など、自分で幅を数えるアプリとはずれることがある」「wide にするとカーソルが乱れるアプリがある」

**ずれたときの症状**（ソースと #17510 の説明からの整理）:

| ConPTY | ずれの出方 |
|---|---|
| OS 同梱（旧方式） | conhost の画面を描き直して送るので、**VT のアプリの出力も conhost の幅で組み直される**。eaw・絵文字の幅が違うと行の残りが右にずれ、描き直しのたびに崩れる（claude での古典的な桁ずれ） |
| 再配布版 1.22 以降 | VT はそのまま流れるので、**VT だけで描くアプリ（claude・vim in wsl・yazi）はアプリと端末が合っていればずれない**。conhost の幅が効くのは、コンソール API でカーソルを動かすアプリ（cmd の行編集、PSReadLine、nyagos）、conhost が出す `CUP`（CPR での位置合わせ、リサイズ）、`GetConsoleScreenBufferInfo` を読むアプリ |

### 2.7 EmbTerm の設計への影響（T-021）

1. **再配布版の `conpty.dll` + `OpenConsole.exe` を同梱し、優先して使う**（T-007）。OS 同梱では OSC 52/9/777/7 の位置ずれ、OSC 8 の書き換え、DEC 2026 の無効化、
   Kitty graphics・Sixel の欠落、DA1 / DECRQM が conhost の答えになる、と要件（§5.3）の多くが満たせない。**版は 1.25 以降**を勧める
   （eaw 2 桁のフラグ、未知のシーケンスの後の位置合わせ、DA1 を待たない起動）。ghostel の同梱物（1.24）は使えるが APC の後にずれる
2. **`OpenConsole.exe` が見つからないと黙って OS 同梱になる**ので、使っている版を確かめる手段を持つ。
   簡単なのは**起動直後の出力に `CSI c`（DA1 の要求）が来るか**（再配布版は来る、OS 同梱は来ない）。ログに出す
3. **端末（libghostty-vt）が問い合わせに答える経路を必ず作る**: DA1・DA2・XTVERSION・DECRQM・DECRQSS・CPR・`CSI 14/16/18 t`・OSC 4/10/11 の `?`・
   Kitty graphics の `a=q`。答えは ConPTY の入力パイプに書く。**CPR は正しい位置を遅れずに返す**（1.25 以降は conhost が位置合わせに使う）
4. **DA1 で名乗る属性は libghostty-vt が本当にできるものだけにする。** 28（矩形操作）を名乗ると conhost が DECCRA / DECFRA を出す。
   4（Sixel）を名乗ると yazi が Sixel を選ぶ（§4）。ghostel は `CSI ?62;22c`（VT220 + ANSI color）相当
5. 起動時の `CSI 1t`、終了時の `CSI ?9001l` などは端末の状態として持つだけでよい
6. OSC 7 / 8 / 9 / 777 / 52 は libghostty-vt から受けて module → elisp に通知する（T-039）。素通しされることは確認済み

### 2.8 実測で確かめるべきこと（T-021）

- 目標のアプリを EmbTerm（または付録 A の手法）で動かし、**再配布版 1.25 で claude の DEC 2026・OSC 9/777 が来るか**、OS 同梱との差
- **CPR に答えなかったとき・遅れたときの 1.25 の振る舞い**（アプリが止まるか、何秒待つか）。リリースノートでは「再送しない」だけ
- `PSEUDOCONSOLE_INHERIT_CURSOR` を使うか（WezTerm は使う、ghostel は使わない）。使うと起動時に CPR + DA1 を最大 1 秒待つ
- `ConptyReleasePseudoConsole` と `ClosePseudoConsole` の組み合わせで、子の終了時に `OpenConsole.exe` が残らないか（T-041）
- OS 同梱の conhost が新方式に変わる時期（Windows Update で変わりうる）。起動時の `CSI c` の有無で見分けられる

---

## 3. T-022: キーとマウスの送り方

### 3.1 分かったこと

**win32-input-mode**（[仕様 #4999](https://github.com/microsoft/terminal/blob/main/doc/specs/%234999%20-%20Improved%20keyboard%20handling%20in%20Conpty.md)）:

- **ConPTY が端末に求める**（起動時に必ず `CSI ?9001h`）。端末が対応していれば、キーを
  `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`（`KEY_EVENT_RECORD` そのもの: 仮想キー・スキャンコード・UTF-16 の文字・押した/離した・修飾キーの状態・繰り返し回数）で送る
- 対応しなければ普通の VT で送ればよい。conhost が VT を解析して `INPUT_RECORD` を作る（`InputStateMachineEngine`）が、
  **VT で表せない区別（Ctrl+Space、Shift+Enter、キーを離した、AltGr、テンキー）は失われる**

**conhost が入力をアプリに渡すとき**（ソース・実測）:

| アプリの入力モード | win32-input-mode の列 | 普通の VT（矢印など） | 知らない CSI（KKP の `CSI 97;5u` など） | SGR マウス |
|---|---|---|---|---|
| **コンソールのアプリ**（`ReadConsoleInput` で読む: cmd・PSReadLine・ネイティブ vim など） | **そのまま `KEY_EVENT` になる**（いちばん正確） | キーに直す | **OS 同梱は捨てる。再配布版は文字として流れる（ゴミ文字になる）**（実測） | `MOUSE_EVENT` に直す（`ENABLE_MOUSE_INPUT` のとき。座標は 0 始まり） |
| **VT 入力のアプリ**（`ENABLE_VIRTUAL_TERMINAL_INPUT`: wsl.exe など） | conhost が**レガシーの xterm 形式の VT に直して**渡す（`a` → `a`、Ctrl+C → `0x03`。**KKP には直さない**: ConPTY では KKP を強制的に無効にしている、`VtIo.cpp` の `ForceDisableKittyKeyboardProtocol`） | そのまま | **そのまま**（実測。OS 同梱も同じ） | そのまま |

**Kitty keyboard protocol と ConPTY**（ソース）:

- アプリが出す `CSI > flags u` などは ConPTY では conhost が処理せず、**端末に素通しされる**（1.22 以降。OS 同梱でも位置がずれて届く）
- **Windows Terminal の方針**（`src/terminal/input/terminalInput.cpp`）: 「win32-input-mode が有効で、**KKP のフラグが 0 のとき**は win32-input-mode で送る。
  KKP のフラグが立っていたら KKP で送る」。つまり KKP を求めた VT アプリには KKP の列が VT 入力経由でそのまま届き、
  それ以外（コンソールのアプリ、KKP を求めない VT アプリ）は win32-input-mode → conhost が翻訳、になる
- WT 1.25 で KKP に対応した目的の一つは「Shift+Enter など、エージェント型の CLI での区別」（[devblog](https://devblogs.microsoft.com/commandline/windows-terminal-preview-1-25-release/)）。
  1.25.2733 / 1.26 で KKP の auto-repeat・dead key・AltGr・テンキーの Enter などの修正、1.25.3131 / 1.26.3133 で「代替画面との切り替えで KKP のモードを保つ」修正（[releases](https://github.com/microsoft/terminal/releases)）

**フォーカス**: ConPTY は `CSI ?1004h` を必ず求める。端末は `CSI I` / `CSI O` を送る。conhost はそれを受けて、アプリが 1004 を求めていればアプリにも渡す。

### 3.2 アプリごとに何が届くか

入力モードは**推測を含む**（★は実測で確かめる）。

| アプリ | 入力の読み方 | EmbTerm が送るべきもの | 備考 |
|---|---|---|---|
| cmd | `ReadConsoleW`（conhost の行編集） | win32-input-mode | 行編集・履歴は conhost がやる |
| PowerShell 5.1 / pwsh（PSReadLine） | `Console.ReadKey` → `ReadConsoleInput` | win32-input-mode | Ctrl+Space（補完メニュー）、Shift+Enter、Alt 系は win32-input-mode でないと区別できない |
| nyagos | ★ `ReadConsoleInput` か VT 入力か未確認 | win32-input-mode | `M-x shell` で動かなかった件は ConPTY で解決する見込み |
| vim（Windows ネイティブ） | `ReadConsoleInput`（コンソールのマウスも） | win32-input-mode + SGR マウス | `Esc` は `VK_ESCAPE` で届くので遅延は vim 側の設定次第 |
| wsl bash と中のアプリ（vim in wsl など） | wsl.exe が VT 入力モードで読み、Linux 側へ流す | win32-input-mode（conhost が xterm 形式に直す）。アプリが KKP を求めたら KKP | KKP は素通しで Linux 側に届く（実測は VT 入力モードで） |
| git bash（MSYS2） | ★ Cygwin の console 実装（`ReadConsoleInput` を自分で VT に直す / VT 入力モード）未確認 | win32-input-mode | |
| claude（Claude Code） | ★ 未確認（Bun / Node の raw モードの実装次第） | 通常は win32-input-mode、claude が `CSI >5u` を出したら KKP | 過去の実測で claude は `CSI >5u`（KKP: disambiguate + alternate keys）・`CSI <u`・`CSI >4;2m`（modifyOtherKeys）・`CSI >0q`・`CSI ?2031h` を出す（`~/.emacs.d/docs/claude/emacs-claude-pty-proxy-study.md`）。**コンソールのアプリとして読んでいると KKP の列はゴミ文字になる**ので、ここは要実測 |
| yazi | ★ 未確認（自前の `yazi-term` / `yazi-tty`） | 同上 | 起動時に `CSI ?u` で KKP を問い合わせる |

### 3.3 マウス（実測・ソース）

- **VT のアプリ**（vim in wsl、yazi、claude）: アプリが出す `CSI ?1000h`〜`?1006h` は素通しで端末に届く。端末が送る SGR マウスは VT 入力のアプリへそのまま届く（実測）
- **コンソールのアプリ**: `SetConsoleMode` で `ENABLE_MOUSE_INPUT` を立てると conhost が端末に `CSI ?1003;1006h` を出す。端末が送った SGR マウスは `MOUSE_EVENT` になる（実測: `CSI <0;5;5M` → 座標 (4,4)、左ボタン）
- つまり**マウスは ConPTY を通る**。端末は「どのモードが有効か」を libghostty-vt の状態で持ち、有効なときだけ報告する

### 3.4 EmbTerm の設計への影響（T-009）

1. **win32-input-mode を実装する**（ghostel はしていない: [ghostel.md](ghostel.md) §6.3）。cmd・PSReadLine・nyagos・ネイティブ vim の修飾キーの区別には要る。
   libghostty-vt のキーエンコーダは win32-input-mode を持たない（推測。Ghostty は Windows の端末ではないため）ので、**自前で組む**。形は単純（6 個の数）
2. win32-input-mode には**仮想キー・スキャンコード・UTF-16 の文字・修飾キーの状態**が要る。
   - **案 B（子 HWND が `WM_KEYDOWN` / `WM_CHAR` を受ける）**ならそのまま揃う（`lParam` のスキャンコード、拡張キーのビット、`GetKeyState`）
   - **案 A（Emacs のキーマップで受ける）**だと Emacs のイベント（`C-S-<return>` など）から `VkKeyScanW` / `MapVirtualKeyW` で**作り直す**ことになる。
     キーを離したイベント・AltGr・テンキーの区別は失われる。実用上は「押した + 離した」を両方合成すれば足りる見込み（推測）
3. **送り方の切り替え**（Windows Terminal と同じ）:
   - libghostty-vt が持つ KKP のフラグが 0 でない → **libghostty-vt の KKP エンコーダで送る**
   - そうでなく、ConPTY が `?9001h` を求めている → **win32-input-mode**
   - それ以外（ConPTY でない、デバッグ用など）→ libghostty-vt のレガシーのエンコード
   - KKP のフラグが立ったまま（アプリが pop せずに落ちた）でコンソールのアプリに戻ると、**ゴミ文字が入る**（再配布版、実測）。
     子プロセスが変わった・代替画面を抜けた、などで戻す安全策を考える（WT 1.25.3131 の修正も参照）
4. ブラケットペーストは、アプリが `?2004h` を出したとき（libghostty-vt の状態）だけ `CSI 200~` で包む。コンソールのアプリには文字として送る（推測。要実測）
5. フォーカスの出入りで必ず `CSI I` / `CSI O` を送る（ConPTY が 1004 を求めているため）
6. `Esc` と `C-[`: win32-input-mode ならコンソールのアプリには別のキーとして届く。VT のアプリには conhost がどちらも `ESC` にする。区別したいアプリは KKP を求めるので、そのときは区別できる

### 3.5 実測で確かめるべきこと（T-022）

- **各アプリの入力モード**（claude・nyagos・git bash・yazi が `ENABLE_VIRTUAL_TERMINAL_INPUT` を立てるか）。
  付録 A の子プロセスを差し替えるか、EmbTerm のデバッグ用 exe で「KKP の列を送ってゴミ文字が出るか」を見る
- claude で **Shift+Enter**（改行）・`Esc`・`C-c`・日本語の確定文字列が、win32-input-mode / KKP のどちらでも期待どおりか
- PSReadLine で Ctrl+Space・Alt+. ・Ctrl+矢印、cmd で F7（履歴の窓。conhost が描く）
- IME の確定文字列（T-010）を win32-input-mode で送るとき、`Vk = 0`（`VK_PACKET` 相当）+ `Uc` の形でよいか。サロゲートペアの扱い（1.25 で変更あり: [#19817 のコメント](https://github.com/microsoft/terminal/pull/19817)）
- 長い貼り付け（数万文字）を win32-input-mode で送ったときの速さ（1 文字 2 列になるので重い。普通の文字列で送るほうがよい可能性）

---

## 4. T-023: yazi の画像プレビュー

### 4.1 yazi の判定（ソース: yazi v26.9.1）

1. **環境変数でブランドを決める**（`yazi-emulator/src/brand.rs`）: `TERM`（`xterm-kitty`・`xterm-ghostty` など）、`TERM_PROGRAM`（`WezTerm`・`ghostty`・`vscode`・`mintty` など）、
   特有の変数（`WT_Session` → Microsoft、`WEZTERM_EXECUTABLE`、`KITTY_WINDOW_ID`、`GHOSTTY_RESOURCES_DIR` など）
2. **問い合わせ**（`yazi-emulator/src/probe.rs`）: 起動時に `CSI ?996n`、OSC 11 `?`、カーソルの形、`CSI >q`（XTVERSION）、
   KGP の問い合わせ（APC `a=q,t=d` と `a=q,t=s` = Windows の名前付き共有メモリ）、`CSI 16t`、OSC 5522、`CSI ?u`、`CSI 0c`（DA1）を出し、**DA1 の答えを 3 秒待つ**
   - XTVERSION の答えに `kitty` / `WezTerm` / `ghostty` / `foot` / `iTerm2` などが含まれればブランドを上書き
   - DA1 の属性に **4 があれば Sixel 可**、KGP の問い合わせに OK が返れば KGP 可
3. **ドライバを選ぶ**（`yazi-adapter/src/drivers/drivers.rs`）:

| ブランド | ドライバの順 |
|---|---|
| Kitty・**Ghostty**・Rio | **Kgp**（Unicode placeholder `U+10EEEE`、`U=1,C=1`） |
| WezTerm・iTerm2・VSCode・Tabby・Hyper | Iip（iTerm2 の OSC 1337）、Sixel |
| Microsoft（Windows Terminal） | Sixel |
| Konsole | KgpOld |
| Mintty | Iip |
| **不明** | 問い合わせの結果で: KGP と Sixel → **Sixel**、KgpOld / KGP だけ → **KgpOld**（直接配置 `a=p`、`C=1`）/ Sixel だけ → Sixel / どちらも無い → X11・Wayland・**Chafa** |

4. **セルのピクセル**: Windows では `TERM.dimension()` のピクセルが 0（手元の `ya env` でも `width: 0, height: 0`）なので、
   **`CSI 16t` の答えが無いと画像の大きさを決められない**（`yazi-emulator/src/dimension.rs`）
5. Windows（と tmux）では「ConPTY でカーソルがずれることがある」ので、画像を出す前に `CUP` を 3 回出す回避策がある（`emulator.rs` の `move_lock`、`cfg!(windows)`）
6. 手元の診断は `yazi --debug` ではなく **`ya env`**（26.x）。WezTerm の中では `Brand.from_env: WezTerm`、`Drivers.matches: Iip` だった

### 4.2 端末ごとの経路（2026-11 時点）

| 端末 | yazi の判定 | ConPTY | 画像 |
|---|---|---|---|
| Windows Terminal 1.22 以降 | `WT_Session` → Microsoft → **Sixel** | 自前の新方式 | **出る**（WT は 1.22 で Sixel に対応）。Kitty graphics は WT 未対応（PR #20872 がレビュー中） |
| WezTerm（nightly） | `TERM_PROGRAM=WezTerm` → **Iip** | 同梱 1.22.2502 | OSC 1337 は 1.22 で素通し（実測）なので**出る見込み**（ユーザー環境で要確認）。WezTerm の Kitty graphics on Windows の issue [#1673](https://github.com/wezterm/wezterm/issues/1673) は 2026-08 時点でも open（原因として挙げられた ConPTY の側は直っている） |
| ghostel（1.24 同梱） | XTVERSION を設定しておらず libghostty の既定名が返る（[ghostel.md](ghostel.md)）。名前に `ghostty` が含まれれば **Kgp**（**推測**、要確認）。ghostel のサイトは「yazi は Unicode placeholder を使う」と書く | 同梱 1.24 | ghostel は Kitty graphics を Emacs の画像で描く。**Windows で出るかの報告は見つからなかった**。1.24 は APC を素通しするので出る見込み（推測）。U=1 + C=1 ならカーソルを動かさないので 1.24 のずれも起きにくい |
| OS 同梱の ConPTY を使う端末 | — | 旧方式 | **Kitty graphics・Sixel とも出ない**（APC・DCS が捨てられ、問い合わせの答えも返らない）。OSC 1337 は位置がずれて届く |

### 4.3 ConPTY を通る端末で Kitty graphics は出るか

- **出る。条件は再配布版の ConPTY（1.22 以降）。** 送り（APC）も、問い合わせの答え（APC `OK`）も通ることを実測した（§2.2、§2.3）。
  他の端末の実測も一致する（[cleat #235](https://github.com/flotilla-org/cleat/pull/235): 1.24.260710001 で通る、OS 同梱で落ちる）。
  [wintty #263](https://github.com/deblasis/wintty/issues/263)（2026-04）の「APC はどの版でも conhost に食われる」は、手元の 1.22〜1.26 の実測と合わない
- **conhost は Kitty graphics を知らない**（main には未実装）。直接配置でカーソルが動くと conhost の位置とずれる。
  1.25 以降は未知のシーケンスの後に CPR で位置を合わせ直す（実測）。**1.24 以前はずれたまま**（#19926）
- **Unicode placeholder（yazi の Kgp）は ConPTY と相性がよい**: placeholder はただの文字（`U+10EEEE` + 結合文字、前景色に画像の ID）なので
  conhost のバッファにも残り、スクロール・再描画でも消えない。`U+10EEEE` は私用面で eaw = A なので、**eaw を 2 桁にすると conhost と端末で幅が変わりうる**（推測）
- **直接配置（KgpOld）** は画像そのものが conhost のバッファに無いので、conhost が画面を描き直す場面（コンソール API のアプリとの切り替え、リサイズ）で
  上書きされて消えうる（[Flare #35](https://github.com/Maxaubert/Flare/issues/35) の報告。未検証）

### 4.4 EmbTerm の設計への影響

1. **Kitty graphics は可能。再配布版 ConPTY（1.25 以降）が前提**（T-007 と同じ結論）
2. libghostty-vt の Kitty graphics のデコード結果（配置・画像）を子 HWND に描く（Direct2D のビットマップ）。Unicode placeholder の走査は ghostel が自前でやっている（[ghostel.md](ghostel.md) §2.7）
3. **答えるべき問い合わせ**: KGP の `a=q`（OK）、`CSI 16t`（セルのピクセル。DPI の変化にも追従）、`CSI 14t` / `18t`、DA1（**4 は名乗らない**: libghostty-vt / Ghostty は Sixel を持たない。名乗ると yazi は Sixel を選ぶ）
4. **yazi に Kgp（placeholder）を選ばせるか、KgpOld（直接配置）で済ませるか**:
   - 何もしなければブランドは「不明」→ KGP が OK なら **KgpOld**
   - XTVERSION の答えに `ghostty` を含めれば **Kgp**（例: `EmbTerm (libghostty-vt …)`）。libghostty-vt を使っているので名乗りとして嘘ではないが、
     **他のアプリが「Ghostty だ」と思って Ghostty 専用の機能を使う**おそれがある。要判断
   - placeholder のほうが ConPTY でのずれ・消えに強いので、**Kgp を選ばせる案を推す**（推測にもとづく判断。実測で比べる）
5. `t=s`（共有メモリ）: yazi は Windows の名前付きファイルマッピング（`yazi-…`）を作る。EmbTerm が `OpenFileMappingW` で読めば対応できる。
   libghostty-vt の `t=s` は POSIX の共有メモリ前提とみられ（推測）、そのままでは Windows で動かない。**問い合わせに失敗を返せば yazi は base64（`t=d`）に戻る**ので、最初は非対応でよい
6. Sixel・iTerm2（OSC 1337）は libghostty-vt に無いので対象外（要件どおり Kitty graphics だけ）

### 4.5 実測で確かめるべきこと（T-023）

- **ghostel（Windows、1.24 同梱）で yazi の画像が出るか**、`ya env` の `Brand` / `Drivers.matches` が何になるか、libghostty の XTVERSION の既定の文字列
- EmbTerm で Kgp と KgpOld の両方を試し、**スクロール・リサイズ・cmd との行き来**で画像が残るか消えるか
- 再配布版 1.24 と 1.25 で、KgpOld のときにカーソルがずれるか（#19926）
- yazi の起動時の問い合わせ（DA1 を 3 秒待つ）が EmbTerm で即座に終わるか

---

## 5. EmbTerm の決定への影響（まとめ）

| 決定 | この調査から言えること |
|---|---|
| **T-007 PTY** | **再配布版 `conpty.dll` + `OpenConsole.exe` を同梱して優先する（OS 同梱は退避先）。版は 1.25 以降（NuGet 1.25.260930003）。** `conpty.dll` の隣か `x64\` に `OpenConsole.exe` を置く。無いと黙って OS 同梱になるので、起動時の `CSI c` の有無で判別してログに出す。フラグは既定（0 = 書記素・eaw 1 桁）、eaw 2 桁の設定時だけ 0x20。0x2 / 0x4 / 0x8 の旧フラグは使わない |
| **T-009 キー入力** | **win32-input-mode を実装する**（仮想キー・スキャンコード・文字・修飾キーが要る）。**案 B（子 HWND が `WM_KEYDOWN` を受ける）のほうが素直**。案 A なら Emacs のイベントから仮想キーを合成する。アプリが KKP を求めたら libghostty-vt の KKP エンコーダに切り替える（Windows Terminal と同じ）。フォーカスの `CSI I/O` を送る |
| **T-011 文字幅** | 再配布版の既定（書記素・eaw 1）は libghostty-vt の既定（eaw 1）・claude（`①` を 1）と同じ向き。OS 同梱は結合文字・ZWJ で合わない。1.22 以降は VT のアプリはずれにくく、ずれるのはコンソール API のアプリ（cmd・PSReadLine・nyagos）と conhost の `CUP` |
| **T-012 eaw の既定** | 既定 1 桁は ConPTY・libghostty-vt と一致する。**2 桁を選べるようにするには ConPTY 1.25 の 0x20 と libghostty-vt の設定の両方が要る**。ConPTY は 2 桁でも罫線を 1 桁に保つので、libghostty-vt 側も同じ例外を持たないとずれる |
| **Kitty graphics** | **可能**（再配布版の ConPTY が条件）。`CSI 16t` と KGP の問い合わせに答える。yazi に Kgp（placeholder）を使わせるには XTVERSION に `ghostty` を含める必要がある（要判断） |

---

## 6. 出典

**microsoft/terminal**

- [PR #17510 A minor ConPTY refactoring: Goodbye VtEngine Edition](https://github.com/microsoft/terminal/pull/17510)（2024-08-01 マージ）
- [PR #16954 Remove VtApiRoutines](https://github.com/microsoft/terminal/pull/16954)（2024-03-28、passthrough モードの削除）
- [Issue #1173 ConPTY passthrough](https://github.com/microsoft/terminal/issues/1173)
- [Issue #19926 cursor position desync after APC](https://github.com/microsoft/terminal/issues/19926)（2026-03、修正 #20009）
- [PR #19864 ambiguous-width policy](https://github.com/microsoft/terminal/pull/19864)（2026-02-27）
- [PR #19817 Kitty Keyboard Protocol](https://github.com/microsoft/terminal/pull/19817)
- [PR #20872 Kitty graphics（レビュー中）](https://github.com/microsoft/terminal/pull/20872)、[PR #20885（close）](https://github.com/microsoft/terminal/pull/20885)
- [win32-input-mode の仕様 #4999](https://github.com/microsoft/terminal/blob/main/doc/specs/%234999%20-%20Improved%20keyboard%20handling%20in%20Conpty.md)
- リリースノート: [v1.22.2362.0](https://github.com/microsoft/terminal/releases/tag/v1.22.2362.0)、[v1.25.622.0](https://github.com/microsoft/terminal/releases/tag/v1.25.622.0)、[v1.26.2734.0](https://github.com/microsoft/terminal/releases/tag/v1.26.2734.0)、[一覧](https://github.com/microsoft/terminal/releases)、[Windows Terminal Preview 1.25 の devblog](https://devblogs.microsoft.com/commandline/windows-terminal-preview-1-25-release/)、[v1.14（passthrough の導入）](https://newreleases.io/project/github/microsoft/terminal/release/v1.14.1432.0)
- ソース（main、2026-11 時点）: [`src/host/VtIo.cpp`](https://github.com/microsoft/terminal/blob/main/src/host/VtIo.cpp)、[`src/host/_stream.cpp`](https://github.com/microsoft/terminal/blob/main/src/host/_stream.cpp)、[`src/host/outputStream.cpp`](https://github.com/microsoft/terminal/blob/main/src/host/outputStream.cpp)、[`src/host/getset.cpp`](https://github.com/microsoft/terminal/blob/main/src/host/getset.cpp)、[`src/terminal/adapter/adaptDispatch.cpp`](https://github.com/microsoft/terminal/blob/main/src/terminal/adapter/adaptDispatch.cpp)、[`src/terminal/parser/InputStateMachineEngine.cpp`](https://github.com/microsoft/terminal/blob/main/src/terminal/parser/InputStateMachineEngine.cpp)、[`src/terminal/input/terminalInput.cpp`](https://github.com/microsoft/terminal/blob/main/src/terminal/input/terminalInput.cpp)、[`src/winconpty/winconpty.cpp`](https://github.com/microsoft/terminal/blob/main/src/winconpty/winconpty.cpp)、[`src/inc/conpty-static.h`](https://github.com/microsoft/terminal/blob/main/src/inc/conpty-static.h)、[`src/types/CodepointWidthDetector.cpp`](https://github.com/microsoft/terminal/blob/main/src/types/CodepointWidthDetector.cpp)
- [NuGet Microsoft.Windows.Console.ConPTY](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY)

**yazi**（v26.9.1）

- [`yazi-emulator/src/brand.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-emulator/src/brand.rs)、[`probe.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-emulator/src/probe.rs)、[`emulator.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-emulator/src/emulator.rs)、[`dimension.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-emulator/src/dimension.rs)
- [`yazi-adapter/src/drivers/drivers.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-adapter/src/drivers/drivers.rs)、[`kgp.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-adapter/src/drivers/kgp.rs)、[`kgp_old.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-adapter/src/drivers/kgp_old.rs)、[`yazi-tty/src/sequence/query.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-tty/src/sequence/query.rs)、[`yazi-ffi/src/shm/windows.rs`](https://github.com/sxyazi/yazi/blob/v26.9.1/yazi-ffi/src/shm/windows.rs)

**ghostel・他の端末**

- [ghostel のサイト](https://dakra.github.io/ghostel/)（Windows・Kitty graphics の節）、[`src/handler.zig`](https://github.com/dakra/ghostel/blob/main/src/handler.zig)、[`src/ConPtyProcess.zig`](https://github.com/dakra/ghostel/blob/main/src/ConPtyProcess.zig)。EmbTerm 側の読み取りは [ghostel.md](ghostel.md)
- [wintty #263](https://github.com/deblasis/wintty/issues/263)（2026-04、ConPTY bypass）、[cleat #235](https://github.com/flotilla-org/cleat/pull/235)（2026-09、同梱 ConPTY で APC / Sixel が通る）、[shelldeck #9](https://github.com/mickaelfrancois/shelldeck/issues/9)（2026-11、claude の桁ずれと同梱 ConPTY）、[Flare #35](https://github.com/Maxaubert/Flare/issues/35)、[WezTerm #1673](https://github.com/wezterm/wezterm/issues/1673)

---

## 付録 A: 実測の方法

[conpty-probe/](conpty-probe/) に置いた。Python（ctypes）だけで動く。

- `harness.py`: パイプ 2 本と擬似コンソールを作り（`kernel32!CreatePseudoConsole` か、指定した `conpty.dll` の `ConptyCreatePseudoConsole`）、
  `child.py` を `STARTF_USESTDHANDLES`（ハンドルは NULL）で起動する。出力をすべて記録し、問い合わせ（DA1・CPR・XTVERSION・DECRQM・DA2・`CSI ?u`・KGP `a=q`・`CSI 16t`・OSC 11・DECRQSS・`CSI ?996n`）に決まった答えを返す。
  子の合図（`<READY-VT>` / `<READY-CON>`）で、win32-input-mode の `a` と Ctrl+C、KKP の `CSI 97;5u`、SGR マウス、`x` を送る
- `child.py`: `CONOUT$` / `CONIN$` を開き、`ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN` で各シーケンスと文字を書き、
  前後のカーソル位置（= conhost の数えた桁）を記録する。次に VT 入力モード、続けてコンソール入力モード（`ENABLE_MOUSE_INPUT`）で `ReadConsoleInputW` の中身を記録する
- 使い方: `python -I harness.py inbox x <出力名>` / `python -I harness.py bundled <conpty.dll のパス> <出力名> [フラグ]`。
  `<出力名>.out.txt`（受け取った列。`ESC` は `\e`）と `<出力名>.child.log`（子の記録）ができる
- 測った版: OS 同梱（26300）、ghostel 同梱 1.24.2605.12001、WezTerm 同梱 1.22.2502.04002、NuGet 1.25.260930003・1.26.260930004-preview（署名を確かめて一時ディレクトリで使った）

**この方法の限界**: 子は Python の 1 プロセスで、目標のアプリそのものではない。入力モードやタイミングはアプリごとに違うので、§2.8・§3.5・§4.5 はアプリで確かめる。
