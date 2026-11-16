# T-137 Windows で、確定した直後に始めた変換の未確定文字列が前の位置に出る

- 日付: 2026-11-16
- **TODO T-137 の記録**（利用者が claude で日本語を打っていて見つけた）
- 結論: **遅れが 2 つ重なっていた。** ①tr-ime に入れた位置は `WM_IME_STARTCOMPOSITION` が来るまで
  IME に届かないが、端末は子 HWND に描くので Emacs の再表示（＝そのメッセージ）が起きない。
  ②端末のカーソルはアプリのエコーが返るまで動かないのに、IME は**確定と同じ打鍵で**次の変換を始める
- 読み直した元資料: [../research/emacs-ime.md](../research/emacs-ime.md) の §2.3・§3.4・§4.4

---

## 1. 症状

claude のプロンプトで「日本語の入力が」と打つとき:

1. 「にほんごの」を打ってスペース → 「日本語の」になる（**この時点ではまだ未確定**）
2. 続けて「にゅうりょく」の `n` を打つ → IME が「日本語の」を確定し、同じ打鍵で `n` の変換を始める
3. このとき未確定文字列（と候補ウィンドウ）が、「日本語の」の次ではなく
   **途中（「日本」の次あたり）**に出る

スペースのあと **Enter で区切ってから** `n` を打つと、正しく「の」の次に出る。
macOS では気にならない。

## 2. 原因

### 2.1 入れた位置は、すぐには IME に届かない

Windows では preedit を描くのは IME 自身で、その位置は tr-ime の `compform_` を
`tr-ime-modadv--set-composition-window` で上書きしている（T-010 の A-2）。

tr-ime のサブクラスが `ImmSetCompositionWindow (compform_)` を呼ぶのは
**`WM_IME_STARTCOMPOSITION` を受けたときだけ**（emacs-ime.md §3.4）。
Emacs は**カーソルを描き直すたびに**自分でこれを投げる（§2.3 の 2、`w32term.c:6685`）ので、
ふつうのバッファなら再表示のたびに追従する。

ところが EmbTerm の端末は**子 HWND に描く**ので、端末のカーソルが動いても Emacs は再表示しない。
`embterm--update-ime` が正しい位置を入れても、Emacs が何か再表示するまで変換ウィンドウは動かない。
emacs-ime.md §2.3 の最後にある「位置が更新されるのは Emacs がカーソルを描き直したときだけ」が
そのまま効いていた。

### 2.2 端末のカーソルは、アプリのエコーが返るまで動かない

`embterm-module-cursor`（`publish_cursor`）が返すのは、確定文字を PTY に送って
claude がエコーを返し、端末が描いた**後**の位置。

emacs-ime.md §4.4 は「位置は変換を始める前に合っていればよい。変換中は端末のカーソルはふつう動かない」
と見積もっていたが、**確定と次の変換開始が同じ打鍵で起きる**この手順では前提が崩れる。

### 2.3 合わさると

`n` の打鍵の中で:

1. IME が「日本語の」を確定（`WM_IME_CHAR` ×4 → Emacs の入力イベント 4 つ）
2. **同じ打鍵の中で** `n` の `WM_IME_STARTCOMPOSITION` → tr-ime が**そのときの `compform_`**
   （＝確定前の、入力欄の先頭）を IME に渡す
3. そのあと Emacs のコマンドループが 4 文字を 1 つずつ処理する（`embterm--self-insert` は
   1 文字 1 回。emacs-ime.md §2.5・§4.5）。**そのたびに Emacs が再表示 → 2.1 の経路で再適用**される
4. 一方 `compform_` は notify / ポーリングで更新されるので、**エコーの到着と競争**になり、
   「日本」まで進んだあたりで最後の再適用が起きて止まる
5. 打ち終わると Emacs はもう再表示しないので、そのあとポーリングが正しい位置を入れても動かない

Enter で区切ると直るのは、確定と次の変換開始が**別の打鍵**になり、間にエコーが届いて
`compform_` が正しくなるから。

macOS で気にならないのは、preedit を module が**毎フレーム自分で描いていて**（T-133、
`crates/render/src/mac/mod.rs` の `push_preedit`）、カーソルが動けば一緒に動くから。

## 3. 直し方

### (a) 入れた位置をその場で反映させる

module に `embterm-module-ime-refresh HWND` を足した。フレームの HWND に
`WM_IME_STARTCOMPOSITION` を `PostMessage` するだけ（`term.rs` の `post_ime_start_composition`）。
**Emacs 自身がカーソルを描くたびにやっているのと同じ手**で、Lisp のスレッドから投げるのも同じ。
変換を始めていないときに投げても、Emacs が毎回そうしているので害は無い。

`embterm--update-ime` は `tr-ime-modadv--set-composition-window` を呼んだ直後にこれを呼ぶ
（上書きをやめる `embterm--ime-reset` でも呼ぶ）。これだけで症状は消えるが、変換を始めた瞬間は
まだ古い位置なので、エコーが届いた数 ms 後に**変換ウィンドウが飛ぶ**。

### (b) 送った文字の幅だけ先読みする

`embterm--self-insert` が文字を送ったら `embterm--ime-advance` を呼び、
**送った文字の表示幅 × セルの幅**だけ x を進めた位置を入れる（`embterm--ime-ahead`）。
これで (a) の飛びも消える。

先読みを捨てる条件:

| 条件 | なぜ |
|---|---|
| module のカーソルの矩形が変わった | エコーが返って本物の位置が分かった（先読みより本物を使う） |
| 1 秒経った（`embterm--ime-ahead-timeout`） | 送った文字をエコーしないアプリ（パスワードの入力など）の保険 |
| 進めた先がウィンドウの本文の幅を越える | アプリが折り返して行が変わるので、x だけ進めても当たらない |
| 制御文字・DEL | 画面に出ないので幅が読めない |
| copy モード | 端末を隠して Emacs のバッファを見ているので、Emacs の位置でよい |

同じ矩形のまま続けて送られたら足し合わせる（確定文字は 1 文字ずつ来るため）。

### (c) 値段

(b) は**文字を送るたび**に `tr-ime-modadv--set-composition-window`（フレームのウィンドウのスレッドへの
`SendMessage`）と (a) の `PostMessage` を 1 回ずつ増やす。もともと `embterm--update-ime` は
**端末のカーソルが動くたび**（notify。文字を打てばエコーで動く）に同じ `SendMessage` をしているので、
打鍵あたり 1 回だったものが 2 回になるだけ。tr-ime が無ければ `embterm--ime-available-p` が nil で
どちらも走らない（macOS も同じ）。

## 4. 確認

`test/ime.el`（`cargo xtask test-emacs ime`）の **18 項目**。tr-ime は `emacs -Q` に無いので
`tr-ime-modadv--set-composition-window` の**代わり**を置いて渡された引数を見る。
端末は `test/key-child.py` の vt モード（届いたキーを記録するだけでエコーしない）なので、
こちらが動かさないかぎりカーソルは止まったままで、先読みの判定が揺れない。

- 位置が端末のカーソル（ウィンドウの左上 + module の矩形）になる（T-010 の回帰）
- 位置を渡したら `embterm-module-ime-refresh` も呼ぶ。HWND が 0 や無いウィンドウなら nil
- 全角 1 文字で 2 セル・半角で 1 セル進む。続けて送ると足し合わさる
- カーソルが動いた・古い・折り返す・制御文字・copy モードでは先読みしない
- 端末を離れたら上書きをやめる（STYLE 0）

**実機の IME での見え方は利用者が確認（2026-11-16 に OK）**。tr-ime が要るので自動確認では見られない。
別の Emacs を起動して、1 章の手順（スペースで変換 → 確定せずに次の `n`）で**ずれなくなった**ことを見た。
