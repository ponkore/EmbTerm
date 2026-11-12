# EmbTerm のシェル統合（PowerShell 7 / Windows PowerShell 5.1）。
#
# EmbTerm の端末の中でだけ読み込む（プロファイルや Emacs 用の初期化スクリプトから。<配布物> は Emacs が読む
# 配布物の場所。~/.emacs.d の設定なら ~/.emacs.d/embterm）:
#
#   if ($env:TERM_PROGRAM -eq 'EmbTerm') { . '<配布物>/etc/embterm.ps1' }
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

# --- プロンプトの境界（OSC 133。T-119） -----------------------------------------
# プロンプトの前に 133;D;終了コード（2 回目から）と 133;A、後に 133;B、Enter でコマンドを始めるときに 133;C を出す。
# EmbTerm はこれで前後のプロンプトへ移る（C-c C-p / C-c C-n）、直前の出力をコピーする（C-c C-o）、終了コードを出す。
# prompt を包むので、starship などで prompt を決めた後で読み込む。2 回読み込んでも包み直さない。
if (-not $global:EmbTermOriginalPrompt) {
    $global:EmbTermOriginalPrompt = $function:prompt
    $global:EmbTermPromptCount = 0
    function global:prompt {
        $ok = $?
        $code = $global:LASTEXITCODE
        $e = [char]27
        $b = [char]7
        $pre = ''
        if ($global:EmbTermPromptCount -gt 0) {
            $c = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
            $pre = "$e]133;D;$c$b"
        }
        $global:EmbTermPromptCount++
        $p = & $global:EmbTermOriginalPrompt
        $global:LASTEXITCODE = $code
        "$pre$e]133;A$b$p$e]133;B$b"
    }
    if (Get-Command Set-PSReadLineKeyHandler -ErrorAction SilentlyContinue) {
        Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
            [Console]::Out.Write("$([char]27)]133;C$([char]7)")
        }
    }
}
