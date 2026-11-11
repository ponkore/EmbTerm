# T-055 端末としての適合を測る（vttest）

- 日付: 2026-11-11
- **TODO T-055 の記録**
- 結論:
  - **ふだん使う範囲（カーソル移動・画面の機能・文字集合・VT102 の挿入と削除・ISO 6429 のカーソル移動と色・代替画面・マウス・鍵盤）は合格。**
    落ちた項目のほとんどは **libghostty-vt が持たない機能**（倍幅・倍高の行、VT52 モード、矩形の操作、いくつかの報告など）で、Ghostty 本体と同じ
  - **ConPTY は出力をほぼそのまま通す。** vttest の生の出力を ConPTY を通さずに libghostty-vt に流した画面と、EmbTerm の画面を 100 画面あまり比べ、すべて同じだった。
    ConPTY が絡むのは **DECCOLM（ConPTY も 132 / 80 桁に変わる）** と **鍵盤（win32-input-mode で送ったキーを ConPTY が VT に直す）** の 2 つ
  - **EmbTerm 側で直したもの: DECCOLM の後に端末の大きさが窓と食い違ったまま残る。** vttest が `CSI ? 40 h` → `CSI ? 3 h` / `l` を送ると、
    libghostty-vt と ConPTY の両方が 132 / 80 桁になり、窓は 80 / 100 桁のままなので、右が切れる・余る。vttest を終えても 80 桁のまま残り、窓の大きさを変えるまで戻らなかった。
    **端末の大きさが窓（ペイン）と違えば窓の大きさに戻す**ようにした（embterm.exe と module の両方。§4）
  - EmbTerm の描画の不足は **文字の点滅（SGR 5）を描かない** の 1 つ（TODO に足す。§6）

---

## 1. 測り方

- vttest 2.7（20230201）を WSL の Ubuntu で動かす。embterm.exe（`cargo xtask dist` の `target/dist/embterm.exe`）、80x24、HackGen Console NF 12pt、再配布版 ConPTY 1.25
- 1 画面ごとに embterm.exe を起動し、`--input` で番号と RETURN を送り、`--screenshot` で PNG とテキストを撮る（vttest は毎回最初から）。
  `vttest -l` のログ（vttest が送った列と、端末からの答え）も残す
- **ConPTY の影響の切り分け**: vttest を小さな中継（Python の `pty` で vttest を内側の pty で動かし、出力をそのまま外へ流しつつファイルにも書く）の下で動かし、
  その生の出力を **ConPTY を通さずに libghostty-vt に流した画面**（et-vt を使う一時的な example）と EmbTerm の画面を比べた
- **鍵盤**: `--input` は PTY に直接書くので EmbTerm のキーの経路を通らない。鍵盤の項目は PowerShell から EmbTerm の窓に `WM_KEYDOWN` / `WM_KEYUP`
  （とマウスの `WM_LBUTTONDOWN` / `UP`）を `PostMessage` で投げて確かめた
- 応答の有無は、vttest のログと、同じ問い合わせを libghostty-vt に直接流したときの `WRITE_PTY` で確かめた
- 画像・テキスト・ログは `target/t055/`（`t<メニュー番号>-<項目>.png` / `.txt` / `.log`、vttest のログ `.vtlog`、生の出力 `.raw`）。
  使った sh・Python・PowerShell の小さな道具はリポジトリに入れていない（§6）。1 画面の撮り方の例（Git Bash から。メニュー 2 の 2 画面目）:

```sh
MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 ./target/dist/embterm.exe --cols 80 --rows 24 --log target/t055/t2-2.log \
  --input-delay 2500 --input-interval 350 --input '2\r' --input '\r' \
  --screenshot target/t055/t2-2.png --screenshot-after 4700 --exit-after 5100 \
  -- wsl.exe -e sh -c "mkdir -p /tmp/t055/t2-2 && cd /tmp/t055/t2-2 && vttest -l"
```
- **Windows Terminal との見比べは省いた。** 自動で撮るにはキーを送る手段が要り、`wt.exe` は引数の `;` を自分の区切りとして読むため最初の試しで窓が 1 つ残った（§7）

## 2. 結果

記号: ✅ 合格 / ❌ 不合格 / △ 一部 / − 試していない。分類: **G** = libghostty-vt の仕様（Ghostty 本体と同じ）、**C** = ConPTY、**U** = UTF-8 の環境（vttest の前提と違う）、
**E** = EmbTerm、**P** = EmbTerm の方針（窓の大きさは端末からは変えない）

### 2.1 メニュー 1〜10

| メニュー | 項目 | 結果 | 分類 | メモ |
|---|---|---|---|---|
| 1 カーソル移動 | 枠（80 桁） | ✅ | | `t1-1` |
| | 枠（132 桁） | ❌ | P | 直す前: libghostty-vt と ConPTY が 132 桁になり、窓（80 桁）から右がはみ出して切れた（`t1-2`）。直した後: 80 桁に戻すので 80 桁に詰めて崩れて描かれる（`t1-2-fix`） |
| | autowrap（80 / 132 桁） | ✅ / ❌ | P | 132 桁は上と同じ |
| | ESC 列の中の制御文字、先頭の 0 | ✅ | | `t1-5`、`t1-6` |
| 2 画面の機能 | 折り返し（DECAWM）、タブの設定と解除 | ✅ | | `t2-1`、`t2-2` |
| | 80 桁の明るい背景（DECSCNM）・暗い背景 | ✅ | | `t2-4`、`t2-6`。132 桁（`t2-3`、`t2-5`）は 1 と同じ |
| | スクロール領域（soft / jump、2 行と全画面） | ✅ | | `t2-7`〜`t2-10`。soft scroll はせず jump と同じ |
| | origin mode | ✅ | | `t2-11`、`t2-12` |
| | SGR の表（暗い背景・明るい背景） | △ | E | 太字・下線・反転・組み合わせは合格。**点滅（SGR 5）を描かない**（`t2-13`、`t2-14`） |
| | カーソルの保存と復元（DECSC / DECRC） | ✅ | | `t2-15` |
| 3 文字集合 | VT100 の文字集合（B・A・0、1・2） | ✅ | | `t3-8`。1・2（DEC の別の ROM）は ASCII（xterm と同じ） |
| | SI / SO | ✅ | | `t3-9`。vttest は `ESC ) A` を「ISO Latin-1」と書くが、94 文字の A は英国の集合で正しい |
| | VT220 のロッキングシフト | △ | U | GL へ呼ぶもの（LS2・LS3）は合格。**GR（LS1R・LS2R・LS3R）は `�`**: vttest が 0xA0〜0xFF を生の 8 ビットで送り、UTF-8 として読めない。libghostty-vt に直接流しても同じ（`t3-10`） |
| | VT220 のシングルシフト（SS2・SS3） | ✅ | | `t3-11` |
| | ソフト文字集合（DECDLD） | − | G | vttest がフォントの指定（`-f`）無しでは出さない。libghostty-vt は持たない |
| 4 倍幅・倍高 | DECDWL / DECDHL / DECSWL（6 画面） | ❌ | G | **libghostty-vt は持たない**。普通の幅で出る（`t4-1`〜`t4-6`） |
| 5 鍵盤 | 矢印（カーソルキーのモード 解除 / 設定 / VT52） | ✅ | C | `CSI A`〜、`SS3 A`〜、`ESC A`〜（`t5-4a`〜`t5-4c`）。EmbTerm は win32-input-mode でキーを送り、**ConPTY が今のモードに合わせて VT に直す** |
| | ファンクションキー F1〜F12 | ✅ | C | F1〜F4 は `SS3 P`〜`S`、F5 は `CSI 15 ~`、F6〜F12 は `CSI 17 ~`〜`CSI 24 ~`（xterm と同じ。vttest は F1〜F4 を「Unknown」と書く。`t5-7a`） |
| | 編集キー（Insert・Delete・Home・End・PgUp・PgDn） | ✅ | C | Home / End は `CSI H` / `CSI F`（xterm と同じ。DEC の Find / Select ではない。`t5-6a`） |
| | LED・自動反復・配列・answerback・制御キー | − | | 人が打つ・見る項目 |
| 6 端末の報告 | ENQ（answerback） | ✅ | G | 何も返さない（libghostty-vt の既定。xterm も既定は空） |
| | LNM（Enter で CR LF / CR） | ✅ | | Enter を投げて確かめた（`t6-2a`） |
| | DSR 5（状態）・DSR 6（カーソルの位置） | ✅ | | `CSI 0 n`、`CSI 5 ; 1 R`（`t6-3`） |
| | DA1 | ✅ | | `CSI ? 62 ; 22 c`（VT220 + 色。T-071 で決めたとおり） |
| | DA2 / DA3 | ✅ | G | `CSI > 1 ; 0 ; 0 c`、`DCS ! \| 00000000 ST`（libghostty-vt の既定） |
| | DECREQTPARM | ❌ | G | 答えない。vttest は答えを待ち続ける（`t6-7`） |
| 7 VT52 モード | 画面の描画（`ESC Y` など） | ❌ | G | **libghostty-vt は VT52 モードを持たない**。`ESC Y` の後の座標が文字として出る（`t7-1`〜`t7-3`）。キーは ConPTY が VT52 の形で送る（5 を参照） |
| 8 VT102 | IL / DL（アコーディオン）、IRM、DCH、ICH（80 桁） | ✅ | | `t8-1`〜`t8-7` |
| | 同じもの（132 桁） | ❌ | P | 1 と同じ（`t8-8`〜`t8-14` は直す前で、右が切れている） |
| 9 VT100 の既知のバグ | A（smooth → jump）、B（スクロール領域）、Wrap around、funny scroll regions | ✅ | | B は 11 行目の倍幅だけ出ない（G） |
| | C・D・F（80 / 132 桁の切り替え） | ❌ | P | 132 桁は 1 と同じ |
| | E（倍幅の行からの移動）、倍幅の行の右半分 | △ | G | 倍幅にならない。行の文字は消えない |
| 10 リセット | RIS、DECSTR | ✅ | | `t10-1b`、`t10-3b` |
| | DECTST | ✅ | G | 何もしない（xterm も同じ） |

### 2.2 メニュー 11（VT220 以降・ISO 6429・xterm）

| メニュー | 項目 | 結果 | 分類 | メモ |
|---|---|---|---|---|
| 11.1 VT220 | DSR: 動作状態 | ✅ | | `CSI 0 n` |
| | DSR: 鍵盤（`CSI ? 26 n`）・プリンタ（`? 15 n`）・UDK（`? 25 n`） | ❌ | G | 答えない（libghostty-vt に直接流しても答えない） |
| | DECTCEM・ECH・DECSCA（DECSED / DECSEL） | ✅ | | `t11-1-2-2`〜`4` |
| | S7C1T / S8C1T | ❌ | G | 8 ビットの C1 で答えない（7 ビットで答える。`t11-1-3`） |
| | DECSTR | ✅ | | |
| | DECUDK | ❌ | G | 持たない |
| 11.2 VT320 | SU / SD（Pan down / up） | ❌ | G | **`CSI 0 S` / `CSI 0 T` を 1 として扱わず、何もしない**（xterm は 1 行）。libghostty-vt に直接流して確かめた。引数が無い・1 のときは正しい（`t11-2-2-1`、`2`） |
| | DECCIR | − | | vttest が「VT200 のみ」と判断して飛ばす（DA1 が 62 のため） |
| | DECTABSR、DECRQTSR、DECRQUPSS、DECRQDE | ❌ | G | 答えない（vttest は待ち続ける） |
| | DECRQSS: SGR・DECSTBM | ✅ | | `DCS 1 $ r 0 m ST`、`DCS 1 $ r 1 ; 24 r ST` |
| | DECRQSS: DECSCA | ❌ | G | `DCS 0 $ r ST` |
| 11.3 VT420 | DECBI・DECFI、余白の中のカーソル移動・CR / HT / LF / FF | ✅ | | `t11-3-2-7`〜`10` |
| | DECRQM（DECLRMM）、DECRQSS（DECSTBM） | ✅ | | |
| | DECRQSS（DECSLRM） | ❌ | G | DECLRMM が無効だと `DCS 0 $ r ST`（有効なら `DCS 1 $ r 1 ; 80 s ST` を返すことを直接確かめた） |
| | DECIC / DECDC、IND / RI、IL / DL、ICH / DCH（余白つき）、BS / CR / TAB | ✅ | | `t11-3-3-9`〜`13` |
| | 矩形の操作（DECCARA・DECCRA・DECERA・DECFRA・DECRARA・DECSERA） | ❌ | G | **持たない**（vttest も「should not work」と書く。`t11-3-6-7`〜`12`） |
| | DSR: 鍵盤・ロケータ・マクロ・チェックサム・データ・セッション・DECRQCRA | ❌ | G | 答えない |
| | DSR: DECXCPR | ✅ | | `CSI ? 2 ; 1 ; 1 R` |
| 11.4 VT520 | DECNCSM | ✅ | | |
| | DECSCUSR（ブロック・下線・縦棒、点滅の有無） | ✅ | | 形は合格（`t11-4-6-2-*`。撮るときは窓にフォーカスが無いのでブロックは枠） |
| 11.5 ISO 6429 のカーソル移動 | HPA・CBT・CHA・CHT・HPR・VPA・CNL・CPL・VPR | ✅ | | `t11-5-1`〜`9` |
| 11.6 ISO 6429 の色 | 8 色の表（明るさ off / on） | ✅ | | on（太字）でも色は明るくしない（Ghostty の既定 `bold-is-bright = false` と同じ） |
| | SGR 0 の解除、BCE（ED / EL / ECH / 索引）、BCE の VT102・画面の機能 | ✅ | | `t11-6-3`〜`8`、`t11-6-6-*` |
| | SGR 22〜27（拡張の表） | △ | E | 隠す（SGR 8）・元の色は合格。点滅は描かない（`t11-6-9`） |
| 11.7 ISO 6429 のほか | SPA（保護領域）、REP | ✅ | | `t11-7-1-2`、`t11-7-2` |
| | SD・SU | ❌ | G | 11.2 と同じ（`CSI 0 T` / `CSI 0 S`） |
| | SL・SR（`CSI Ps SP @` / `SP A`） | ❌ | G | 持たない（`t11-7-4`、`5`） |
| 11.8 xterm | DECRQM / DECRPM | ✅ | | xterm にしか無いモードは「unknown」（`t11-8-2-2`） |
| | タイトル（OSC 0） | ✅ | | 窓のタイトルが変わった |
| | マウス（Normal Mouse Tracking） | ✅ | | クリック 2 回を投げ、`CSI M` の報告と位置が合った（`t11-8-5-4`） |
| | 代替画面（47・1047・1049） | ✅ | | 戻ったときの画面とカーソルの復元も合格（`t11-8-7-*`） |
| | 窓の報告（`CSI 20 t`・`21 t`・`13 t`・`11 t`） | ❌ | G | 答えない（タイトルの報告は安全のため。xterm も既定で無効）。`CSI 14 t`・`18 t`・`19 t` は答える |
| | 窓の操作（dtterm）・フォント・Tektronix | − | | 窓を動かす・アイコンにする類で、EmbTerm は受けない |

## 3. 分類のまとめ

- **libghostty-vt の仕様（G）**: DECDHL / DECDWL、VT52 モード、矩形の操作、DECUDK、DECDLD、S8C1T、SL / SR、`CSI 0 S` / `CSI 0 T`、
  DECREQTPARM・DECTABSR・DECRQTSR・DECRQUPSS・DECRQDE・DSR の VT220 以降の多く・DECRQCRA・窓の報告の一部に答えない、DECRQSS の DECSCA と（DECLRMM が無効なときの）DECSLRM。
  Ghostty 本体も同じで、EmbTerm では直さない。ふだんのアプリ（シェル・vim・less・tmux など）で困るものは無い
- **ConPTY（C）**:
  - 出力は通す（§1 の比べ方で差は無かった）。ただし **DECCOLM は ConPTY 自身も従う**: `CSI ? 40 h` の後の `CSI ? 3 h` で、WSL の `stty size` が 100 → 132 → 80 と変わった
    （EmbTerm は `ResizePseudoConsole` を呼んでいない）
  - 鍵盤: ConPTY が win32-input-mode（9001）を求めるので、EmbTerm はキーをその形で送り、**VT の符号にするのは ConPTY**。DECCKM・VT52・LNM に合わせた符号が届いた
- **UTF-8 の環境（U）**: VT220 の GR（8 ビット）の表示。vttest は 8 ビットの端末を前提にしている
- **EmbTerm の方針（P）**: 132 桁の表示。窓（Emacs のペイン）の大きさは端末からは変えないので、DECCOLM は「窓の大きさに戻す」にした（§4）
- **EmbTerm（E）**: 文字の点滅を描かない（§6）

## 4. 直したこと: DECCOLM の後に窓の大きさへ戻す

- 起きていたこと（直す前の `target/dist/embterm.exe`）:
  - 80x24 の窓で vttest の 132 桁の画面を開くと、libghostty-vt の端末が 132 桁になり、右の 52 桁が窓の外で見えない（`t1-2.png`）
  - **100x24 の窓で vttest のメニュー 1 を通して終えると、端末も ConPTY も 80 桁のまま残った**（`stty size` が `24 80`、100 桁ぶんの `A` が 80 桁で折り返す。`deccolm100.txt`）。
    窓の大きさを変えるまで戻らない（窓の大きさが同じなら同じ桁数とみなして何もしないため）
- 直し方:
  - `et-session` に `Session::size_drift(grid)` を足した（端末の今の大きさが窓から決めた大きさと違えば、今の大きさを返す）
  - embterm.exe の `App::sync_size`（`apps/embterm/src/window.rs`）と module の `sync_sizes`（`apps/embterm-module/src/term.rs`）で、窓の大きさが変わっていなくても
    端末の大きさが違えば `Session::resize` で窓（module は選んでいるペイン）の大きさに戻す。どちらも描く前に毎回呼ばれる場所。ログに
    「端末の大きさが 132x24 に変わった（DECCOLM）ので窓の 100x24 に戻す」と出す
  - et-vt のテスト `deccolm_resizes_terminal` を足した（`CSI ? 40 h` が無ければ変わらず、あれば 132 / 80 桁になる。直し方の前提）
- 確かめたこと:
  - 100x24 の窓で `CSI ? 40 h` `CSI ? 3 h` → `CSI ? 3 l` → `CSI ? 40 l` `CSI ? 3 h` を送り、それぞれの後の `stty size` が **すべて `24 100`**。
    100 桁ぶんの `A` も 1 行に収まる（`deccolm-fix.txt`、`deccolm-fix.log`）
  - vttest の 132 桁の画面は 80 桁に詰めて描かれる（`t1-2-fix.png`）。崩れるが、端末と ConPTY と窓が食い違ったまま残ることは無くなった
  - Emacs（module）でも同じことを確かめた（§5）
- 窓（ペイン）を広げて 132 桁を見せる方法は採らない。Emacs のウィンドウの大きさは Emacs が決めるもので、端末からは変えない（`CSI 8 ; r ; c t` の窓の大きさの要求も受けていない）

## 5. 確かめたこと

- `cargo build`、`cargo test --workspace`（すべて通る。et-vt に 1 件足した）、`cargo clippy --workspace --all-targets`（警告なし）、`cargo xtask dist`
- Emacs の試験（`emacs -Q -l test/X.el`。`cargo xtask dist` の後）: split（NG 0）、kitty（NG 0）、integration（OK 56 / NG 0）、close（NG 0）。
  smoke と keys は OK / NG を数えない試験で、最後まで進み、エラーは無かった。どのログにも DECCOLM の戻しは出ていない（ふだんは働かない）。activate.el は省いた
- Emacs で DECCOLM（別の `emacs -Q` で、端末に `CSI ? 40 h` `CSI ? 3 h` → `CSI ? 3 l` を送る）: `stty size` は 3 回とも `29 109`（ペインの大きさ）。
  module のログに「端末の大きさが 132x29 に変わった（DECCOLM）のでペインの 109x29 に戻す」と 80x29 の同じ行が出た

## 6. TODO に足すべき課題

- **文字の点滅（SGR 5）を描く。** render state のスタイルに `GHOSTTY_STYLE_FLAG_BLINK` があるのに、`et-render` は見ていない。カーソルの点滅（T-035）と同じタイマーで、
  点滅する文字を消す / 出すを交互に描く（Ghostty・Windows Terminal・xterm は点滅させる）。急がない
- **libghostty-vt の上流に知らせるか決める**: `CSI 0 S` / `CSI 0 T` を 1 として扱わない（xterm・ECMA-48 の既定と違う）。ほかの G は「持たない」ものなので知らせる必要は薄い
- **vttest の自動確認を回帰テストにする**: 今回の道具（1 画面ずつ embterm.exe を起動する sh、出力を写す中継、キーを投げる PowerShell、ConPTY を通さない再生）を
  `cargo xtask` から呼べる形にし、合格した画面のテキストを期待値として比べる（T-078 に含めてよい）
- **Windows Terminal と見比べる（手で）**: 自動では省いた。特に DECCOLM（Windows Terminal の窓は変わるか、ConPTY と食い違うか）と倍幅の行

## 7. 気づいたこと

- vttest は DA1 の 62（VT220）を見て、VT320 以降の項目の一部を飛ばす（DECCIR など）。DA1 を上げると矩形の操作などを試すようになるが、持たないので落ちるだけ。T-071 のまま 62 でよい
- vttest は答えの来ない問い合わせ（DECREQTPARM、DSR の多く、窓の報告）で待ち続ける。自動で回すときは、そういう項目の後は vttest を起動し直す
- `--input` は PTY に直接書くので、EmbTerm のキーの経路（win32-input-mode・KKP・キーエンコーダ）は通らない。鍵盤の確認は窓にメッセージを投げる
- Windows Terminal を `wt.exe -w new ... wsl.exe -e sh -c "...; ..."` で開くと、`;` が `wt.exe` の区切りとして読まれ、試しの窓（タイトル `stty size >> $f`）が 1 つ残った。
  閉じるのは止めておいた（利用者の窓と区別できる確かな方法が無いため）。閉じてよい
