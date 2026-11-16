# CLAUDE.md

Claude Code がこのリポジトリで作業するときの約束。
**何を作っているか・どう動くか・どう使うかは書かない**（下の文書を読むこと）。

| 知りたいこと | 読むもの |
|---|---|
| 何ができるか・入れ方・使い方 | [README.md](README.md) |
| 構造・スレッド・入力の流れ | [docs/architecture.md](docs/architecture.md) |
| なぜそうしたか | [docs/adr/](docs/adr/) |
| 要件（確定・決定・案の別） | [docs/requirements.md](docs/requirements.md) |
| **いま残っている課題** | **GitHub Issues**（`gh issue list`）。§3 |
| これまでの課題と決定の経緯 | [docs/TODO.md](docs/TODO.md)（T-001〜T-138 で**凍結**。参照専用） |
| 1 件ごとの作業の記録 | [docs/notes/](docs/notes/) |
| 調査（ConPTY・w32 の IME・libghostty-vt・macOS 移植） | [docs/research/](docs/research/) |

---

## 1. 言語

**日本語で書く。** 応答・コメント・docstring・ドキュメント・コミットメッセージのすべて。
技術用語と識別子（`WM_IME_STARTCOMPOSITION`、`RenderState`、`embterm--update-ime`）は原語のまま。

## 2. 作業の進め方

1. **まず原因を調べる。** 直す前に、どこで何が起きているかを読んで裏を取る。
   推測で直さない。根拠にした場所（ファイル名と行、上流のソース、調査の文書）を書く
2. **GitHub Issue に起こす**（§3）。見つけた課題は、その場で直さないものも含めて必ず残す
3. 直す
4. **自動確認を足す**（§5）。実機でしか見られないもの（IME の見え方、マウスの形、通知の出方）は
   利用者に確認を頼み、結果を記録に書く
5. **記録を書く**（§4）
6. コミットする（本文の `Closes #12` で Issue が閉じる。§6）

## 3. 課題の管理（GitHub Issues）

**課題は GitHub Issues で管理する。TODO.md には新しい課題を足さない。**

- TODO.md は **T-001〜T-138 の記録として凍結**した。参照専用（requirements.md・notes・ADR・過去のコミットが
  T-番号で参照しているので、消さない・付け替えない）
- 凍結したときに未完了だったものは Issue に移してある。TODO.md の該当行には Issue 番号を書き添えてある

### 新しい課題を起こす

`gh issue create` で起こす（`gh` は scoop で入れてある）。

```console
gh issue create --title "確定の直後に始めた変換の位置がずれる" --body-file <本文>
```

- タイトルは**症状**を日本語で短く（直し方ではなく、起きていることを書く）
- 本文は **症状 / 原因（分かっていれば）/ 直し方の案 / 確認のしかた**。分かっていないことは「未調査」と書く
- 作業中に見つけた**別の課題も必ず起こす**。その場で直さないものこそ残す
- 既存の Issue と重なっていないか `gh issue list` で見てから起こす

### 閉じる

- コミットの本文に `Closes #12` と書く（push で自動的に閉じる。§6）
- 直さずに閉じるときは、理由を Issue にコメントしてから `gh issue close`

### Issue 以外に残すもの

- 決定の理由が後で効くものは [docs/adr/](docs/adr/) に 1 枚足す（形は既存の ADR に合わせる）
- 決定したことが requirements.md の【案】に関わるなら、requirements.md も直す
- 原因が 1 行で済まないものは [docs/notes/](docs/notes/) に残す（§4）

Issue は**いま何が残っているか**、ADR と notes は**なぜそうしたか・何が起きていたか**を持つ。
Issue の本文に長い調査を書かず、notes に書いてリンクする。

## 4. 記録（docs/notes/）

原因が 1 行で済まないものは `docs/notes/gh<Issue 番号>-<短い名前>.md` に残す
（`t<番号>-…` は TODO.md の時代のもの。既存のファイル名は変えない）。形は既存のものに合わせる。

```markdown
# #12 ○○が△△になる

- 日付: 2026-11-16
- **[Issue #12](https://github.com/ponkore/EmbTerm/issues/12) の記録**
- 結論: **一文で**

## 1. 症状  ## 2. 原因  ## 3. 直し方  ## 4. 確認
```

- **読んで裏を取ったこと**と**推測**を混ぜない。調査の文書（docs/research/）は【読】【推】で分けている
- 原因が複数重なっているときは ① ② と分けて書く

## 5. 確認

回し方は [README.md §6](README.md#6-開発)。**何を回すかの目安**は次のとおり。

| 直したところ | 回すもの |
|---|---|
| `crates/*` のロジック | `cargo test` |
| VT の解釈・描画 | `cargo test` + `cargo xtask test-screens` |
| `lisp/embterm.el`・module の Lisp から見える関数 | `cargo xtask test-emacs`（関係するものだけでもよい） |
| 入力・IME・マウス | 上に加えて**実機の確認を利用者に頼む** |

- Lisp を直したら `emacs -Q --batch --eval '(byte-compile-file "lisp/embterm.el")'` で警告を見る
  （`.elc` は残さない）
- 自動確認は実際にフレームを開く。`sit-for` を使わず、`run-at-time` で段階を進める形に揃えてある
- **落ちた試験が自分の変更のせいか確かめる**（変更を stash して回す）。関係なければ Issue に起こす
- 既知の失敗は `gh issue list` で確かめる（例: `cargo xtask test-emacs split` のクリックの選択が 3 項目落ちる）

## 6. コミット

- **1 コミット 1 課題**。件名は日本語で「〜する（#12）」
  （例: `確定の直後に始めた変換の位置がずれるのを直す（#12）`。T-番号は凍結前のコミットのもの）
- 本文には、原因と直し方の要点、確認の方法を書き、**最後に `Closes #12`** を入れる（push で Issue が閉じる）
- コミットするのは、記録（notes）を書き、自動確認が通ってから
- **利用者が「コミットして」と言うまでコミットしない**ことがある。指示に従う

## 7. コードの約束

[docs/architecture.md §3](docs/architecture.md) の「約束」は必ず守る。実装の細かいところ:

- **`cargo fmt` はかけない。** 既定の `max_width` と既存のコードが合わず、全体が差分になる。
  周りの書き方に合わせる
- `unsafe` は `crates/vt`（FFI）と OS 固有のモジュールに閉じ込める。外に出さない
- module の境界（`#[defun]`・wndproc・スレッドの本体・`Drop`）は `catch_unwind` で包む。
  `panic = "abort"` にしない
- **Windows と macOS の両方がビルドできる形にする**（`#[cfg(windows)]` / `#[cfg(target_os = "macos")]`）。
  片方だけの機能は Lisp 側でも `fboundp` で守る
- docstring とコメントは日本語。**なぜそうしたか**を書く（何をしているかはコードを読めば分かる）。
  決定に由来するものは T-番号を添える
- 新しいクレートを作ったら `Cargo.toml` の `members` に足す（glob は使わない）

## 8. しないこと

- **バイナリを配布しない。** 手元でビルドする前提（README §3）
- **`target/dist` を普段使いの Emacs に直接読ませない。** `conpty.dll` と `OpenConsole.exe` が
  使用中になり、次の `dist` と自動確認が失敗する
- `vendor/` を git に入れない（`cargo xtask fetch-*` で取る）
- 端末の設定画面・拡張言語を作らない（設定は elisp だけ。requirements.md §5.9）
- 利用者の `~/.emacs.d` を勝手に書き換えない。変更が要るならパッチか手順を示す
