# EmbTerm のシェル統合（PowerShell 7 / Windows PowerShell 5.1）。
#
# EmbTerm の端末の中でだけ読み込む（プロファイルや Emacs 用の初期化スクリプトから）:
#
#   if ($env:TERM_PROGRAM -eq 'EmbTerm') { . 'C:/Apps/embterm/etc/embterm.ps1' }
#
# - embterm_cmd 名前 引数...   Emacs の関数を呼ぶ（OSC 51;E。T-118）。呼べるのは embterm.el の
#                              `embterm-shell-commands' に載っているものだけ
# - embterm_find_file パス...  ファイルを Emacs の別のウィンドウで開く（find-file-other-window）
# - embterm_dired [パス]       ディレクトリを Emacs の別のウィンドウで dired で開く（dired-other-window）
#
# 短い名前にしたければ、読み込んだ後で Set-Alias（例: Set-Alias ef embterm_find_file）。

function global:embterm_cmd {
    # 引数を "..." で包む（\ と " はバックスラッシュで逃がす。Emacs の split-string-and-unquote で戻る）
    $quoted = foreach ($a in $args) { '"' + ([string]$a -replace '\\', '\\' -replace '"', '\"') + '"' }
    [Console]::Out.Write("$([char]27)]51;E$($quoted -join ' ')$([char]27)\")
}

function global:embterm_path([string]$Path) {
    # PowerShell のカレントディレクトリ（$PWD）から絶対パスにする（プロセスの作業ディレクトリとは違うことがある）
    if (-not $Path) { $Path = '.' }
    if (-not [IO.Path]::IsPathRooted($Path)) { $Path = Join-Path $PWD.ProviderPath $Path }
    [IO.Path]::GetFullPath($Path)
}

function global:embterm_find_file {
    foreach ($p in $args) { embterm_cmd find-file-other-window (embterm_path $p) }
}

function global:embterm_dired {
    embterm_cmd dired-other-window (embterm_path ($args | Select-Object -First 1))
}
