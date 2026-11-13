# T-124 段階 0: libghostty-vt を macOS でリンクし、Unix の PTY で画面を作る

- 日付: 2026-11-13
- **TODO T-123 / T-124 の記録**（検討は [research/macos-port.md](../research/macos-port.md)）
- 環境: macOS（Darwin 27.2.0、aarch64）、Rust 1.95、Xcode CLT の clang。**zig は入れていない**
- 結論: **段階 0 は通った。** libghostty-vt は上流の xcframework でリンクでき（zig 不要）、`et-vt` のテストは
  macOS で 31 本すべて通る。`et-pty` の Unix 版（openpty）で子プロセスが動き、`cargo xtask test-screens` の
  回帰テストが macOS でも動くようになった（6 ケース）

---

## 1. やったこと

| | 内容 |
|---|---|
| `crates/vt` | 上流の **xcframework**（`macos-arm64_x86_64/libghostty-vt.a`。universal）をリンク。`build.rs` を OS で分け、macOS では `libc++` も繋ぐ。PNG のデコード（WIC）は Windows だけにして、ほかの OS は何もしない `png_none.rs` に差し替え |
| `xtask` | `fetch-ghostty-vt` を OS で分けた（Windows: zig でビルド / macOS: xcframework を取り出す）。SHA256 の確認を `certutil` と `shasum` で分け、`fetch-conpty` は Windows だけにした。`dist` は macOS ではヘッドレスの `embterm` と `embterm.el`・`etc/embterm.bash` だけ |
| `crates/pty` | **3 つに分けた**: `common.rs`（OS をまたいで同じ形）・`conpty.rs`（今までの Windows の実装をそのまま移動）・`unix.rs`（新規） |
| `crates/session` | `windows` クレートを Windows だけの依存にし、キーの変換（`keys.rs`。Windows の仮想キーに作り込んである）とクリップボードの読み取りを `#[cfg(windows)]` に。既定のコマンドを `et_pty::default_shell()`（Windows は cmd、Unix は `$SHELL`）に |
| `apps/embterm` | **ヘッドレスの動かし方**（`headless.rs`）を足した。macOS では窓を持たず、`--input` を送り、`--screenshot` の隣に画面のテキストを書く |
| `crates/render`・`apps/embterm-module` | いまは Windows だけなので、ほかの OS では**クレートごと空**にした（`#![cfg(windows)]`）。`cargo test --workspace` が macOS でも通る |
| `test/screens` | Unix 用のケース（`cases-unix.txt` と `unix/*.sh`）を足した |

## 2. libghostty-vt（T-124 の山場）

**zig は要らなかった。** 上流のリリースに Apple 向けの配布物がある。

| | |
|---|---|
| 取るもの | `https://release.files.ghostty.org/libghostty-vt/1.0.0/ghostty-vt-1.0.0.xcframework.zip`（16 MB） |
| SHA256 | `2c32b740…a8bc2`（`SHA256SUMS` の値と一致を確認） |
| 中身 | `ghostty-vt.xcframework/macos-arm64_x86_64/{libghostty-vt.a, Headers/}`（ほかに iOS 向け）。`libghostty-vt.a` は **x86_64 + arm64 の universal**、27 MB、C ABI の記号 215 個 |
| 置き場 | `vendor/ghostty-vt/{lib/libghostty-vt.a, include/, VERSION.txt}`（zig でビルドした Windows 版と同じ配置） |
| リンク | `cargo:rustc-link-lib=static=ghostty-vt` + **`cargo:rustc-link-lib=dylib=c++`**。中の highway・simdutf が C++ なので libc++ が要る。それ以外の未解決の記号は libSystem だけ |

**`et-vt` のテストは macOS で 31 本すべて通った**（`cargo test -p et-vt`）。この中には
`ghostty_type_json()` と突き合わせる**構造体の大きさ・揃え・フィールドの位置の照合**が含まれるので、
「ABI が合っている」ことまで確かめられている。直した点は 2 つだけ:

- `types()` が `abi.os == "windows"` を決め打ちしていた → `std::env::consts::OS` と比べる
  （libghostty-vt が名乗る綴りは Rust と同じ。macOS では `macos`）
- WIC の PNG のデコードのテストを `#[cfg(windows)]` に

## 3. Unix の PTY（`crates/pty/src/unix.rs`）

ConPTY 版と同じ形（`Pty` / `PtyOptions` / `ExitStatus` / `CloseReport` / `PtyWriter` / `ProcessWatch`）を出す。

| ConPTY（Windows） | openpty（Unix） |
|---|---|
| `CreatePseudoConsole` + 名前付きパイプ | `openpty` の master / slave |
| 子は `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` | 子は `Command` の `pre_exec` で `setsid` + `ioctl(TIOCSCTTY)`、標準入出力は slave |
| 子孫は Job Object | 子孫はプロセスグループ（子が `setsid` で長）+ 閉じ始めた時点の子孫のスナップショット |
| 閉じる: `ClosePseudoConsole`（CTRL_CLOSE_EVENT）→ 猶予 → `TerminateJobObject` | 閉じる: `killpg(SIGHUP)` → 猶予 → `killpg(SIGKILL)` + スナップショットの子孫に `SIGKILL` |
| 読みは overlapped + `WaitForMultipleObjects` | 読みは `poll`（master と「止まれ」のパイプ） |
| リサイズは `ResizePseudoConsole` | リサイズは `ioctl(TIOCSWINSZ)` |

- `PtyOptions` の `conpty_search` と `flags` は **Unix では使わない**。呼ぶ側（session）を OS で分けずに済むよう欄だけ残した
- master は `FD_CLOEXEC` にする。**子が master を持つと、子が終わっても EOF が来ない**
- Rust は `SIGPIPE` を無視にするので、子では既定に戻す（`head` などで固まらないように）
- 依存に `libc` を足した（Unix だけ）。ConPTY 側が `windows` クレートを使っているのと同じ扱い

**踏んだ落とし穴（実測）**: macOS の `proc_listchildpids` / `proc_listpgrppids` は、
**バッファを渡すと「書いた個数」を返す**（`proc_listpids` のようにバイト数ではない）。
一方 `NULL` を渡した下見では「全プロセスのバイト数」らしい大きな値（964）が返る。
最初これをバイト数として割っていて、子孫がいるのに空に見えた。いまは
「バッファは 0 で埋めて多めに取り、返り値は個数として読み、0 の欄は捨てる」ので、どちらの意味でも壊れない。

**`et-pty` の試験（6 本。`cargo test -p et-pty`）**

| 試験 | 確かめること |
|---|---|
| `echo_runs_and_exits` | 出力が届き、終了コード 0、`closed_by_us` が false |
| `exit_code_is_reported` | `exit 3` が 3 で届く |
| `input_is_echoed_by_the_line_discipline` | `cat` に打った文字が、行の編集のエコーと `cat` の出力で 2 回返る |
| `resize_changes_the_size_the_child_sees` | 子の `stty size` が `24 80`（渡した大きさが伝わっている） |
| `close_stops_the_child_and_its_descendants` | `sh -c 'sleep 60 & sleep 60'` の**子と孫が両方とも**閉じたときに残らない |
| `env_changes_and_foreign_terminal_vars` | `setenv` 相当の設定・削除が効き、親の端末の変数（T-095）は落ちる |

## 4. ヘッドレスの embterm（`apps/embterm/src/headless.rs`）

描画はまだ Windows だけなので、macOS の `embterm` は窓を持たない。時刻の進み方は window.rs のタイマーと同じで、
`--input-delay` → `--input` を `--input-interval` おきに → `--screenshot-after` で撮る → `--exit-after` で閉じる。
画面のテキストは `Terminal::text_rows`（T-119 で入れたもの）で読む。PNG は段階 1（T-125）で。

```text
$ cargo run -p embterm -- --cols 60 --rows 8 --input 'ls\r' --screenshot x.png --screenshot-after 1500 --exit-after 2000
[embterm 0.006] 起動（ヘッドレス）: ["/bin/zsh"] / 60x8 / pid 445 / PTY: openpty
[embterm 1.511] 画面のテキストを保存: x.txt（PNG は段階 1 で）
[embterm 2.013] 閉じ方: 終了の依頼 0 ms / EOF 5 ms / 全員の終了 5 ms / Job の外の子孫 0 / 強制終了 0
```

## 5. 画面の回帰テスト（`cargo xtask test-screens`）

Windows のケース（cmd と vttest）は macOS では動かないので、**`test/screens/cases-unix.txt` を別に作った**。
`cases.txt` は「コマンドを空白で区切る」書き方なので引数に空白を入れられない。そこで確かめたいことは
`test/screens/unix/*.sh` に書き、`/bin/sh そのスクリプト` を動かす（対話シェルを使わないので、
プロンプトの違いで結果が変わらない）。

| ケース | 確かめること | 期待値（目で確認した点） |
|---|---|---|
| `sh-basic` | 文字・日本語・eaw・タブ | `tab:` の後ろが 8 桁ごとのタブ位置に揃う。`日本語 ①★※ ─│┌┐` が桁ずれしない |
| `sh-wrap` | 右端の折り返し | 60 桁で折り返し、2 行目が `0123456789END` |
| `sh-clear` | `CSI 2J` + `CSI H` | `before` が消え `after` だけ |
| `sh-cup` | CUP・DECSTBM（2〜4 行）でのスクロール | 領域の中だけが動き、押し出された `row3-col5` が消え、領域の外の `row5` と `done` は残る |
| `sh-sgr` | 色・太字・下線・true color・反転 | 桁がずれない（テキストだけを比べる） |
| `cat-echo` | **入力の経路**（PTY に送った文字が行の編集でエコーされ、`cat` が返す） | 打った行が 2 回ずつ出る。日本語と eaw も往復する |

- 期待値は `--bless` で作ったあと、**6 件すべて目で見て確かめた**（上の表の「期待値」の列）
- 2 回続けて動かして同じ結果になることを確認（ぶれない）

## 6. 作り方の分け方（T-123）

- **リポジトリは分けない。** `#[cfg]` と、OS ごとのファイル（`conpty.rs` / `unix.rs`、`window.rs` / `headless.rs`）で分ける
- `windows` クレートは `[target.'cfg(windows)'.dependencies]` に移した（`et-vt`・`et-pty`・`et-session`・`embterm`・`et-render`・`embterm-module`）
- まだ Windows しか無いクレート（`et-render`・`embterm-module`）は、ほかの OS では `#![cfg(windows)]` で**空のクレート**にした。
  これで `cargo test --workspace` が macOS でも通る
- **Windows 側を壊していないかは、この Mac から確かめられる**:
  `rustup target add x86_64-pc-windows-gnu` → `cargo check --workspace --target x86_64-pc-windows-gnu`。
  リンクはしないので `ghostty-vt-static.lib` が無くてもよい（build.rs の警告だけ出る）。**型の確認まではできる**ので、
  段階 1 以降も Windows 側の作業の後にこれを通すこと（本当のビルドと試験は Windows 機で）

## 7. 今回の確認

| | 結果 |
|---|---|
| `cargo test --workspace`（macOS） | **41 本すべて OK**（et-vt 31、et-pty 6、ほか 4） |
| `cargo xtask test-screens`（macOS） | **6 ケース OK**（2 回続けて同じ） |
| `cargo check --workspace --target x86_64-pc-windows-gnu` | **エラー 0**（Windows 側の型は壊れていない） |
| Windows 機での本当のビルドと試験 | **未**（段階 1 以降とまとめて確かめる） |

## 8. 残したもの（次の段階で）

| 置いていったもの | いつ |
|---|---|
| PNG のデコード（Kitty graphics。macOS は ImageIO） | T-127 |
| クリップボードの読み取り（NSPasteboard） | T-127 |
| キーの変換（`keys.rs` は Windows の仮想キーに作り込んである） | T-126 |
| 描画（CoreText + Metal）と `--screenshot` の PNG | T-125 |
| Emacs module（NSView の埋め込み） | T-126 |
| Linux（`/proc` から子孫を引く所は書いたが**未確認**） | 当面やらない |
| `embterm` の Unix 版の引数（`--font*` は受け取るが効かない。`--narrow-wide` は捨てる） | T-125 |
