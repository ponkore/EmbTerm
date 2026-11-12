# EmbTerm のシェル統合（bash。Git Bash・MSYS2・WSL）。
#
# EmbTerm の端末の中でだけ読み込む（~/.bashrc から）:
#
#   [ "$TERM_PROGRAM" = EmbTerm ] && . /c/Apps/embterm/etc/embterm.bash      # Git Bash
#   [ "$TERM_PROGRAM" = EmbTerm ] && . /mnt/c/Apps/embterm/etc/embterm.bash  # WSL（WSLENV に TERM_PROGRAM を足す）
#
# - embterm_cmd 名前 引数...   Emacs の関数を呼ぶ（OSC 51;E。T-118）。呼べるのは embterm.el の
#                              `embterm-shell-commands' に載っているものだけ
# - embterm_find_file パス...  ファイルを Emacs の別のウィンドウで開く（find-file-other-window）
# - embterm_dired [パス]       ディレクトリを Emacs の別のウィンドウで dired で開く（dired-other-window）
#
# パスは Emacs（Windows）が開ける形にして渡す（Git Bash は cygpath -w、WSL は wslpath -w）。

embterm_cmd() {
    local a out=""
    for a in "$@"; do
        a=${a//\\/\\\\}
        a=${a//\"/\\\"}
        out+="\"$a\" "
    done
    printf '\033]51;E%s\033\\' "${out% }"
}

embterm_path() {
    local p="${1:-.}"
    p=$(realpath -m -- "$p" 2>/dev/null || printf '%s' "$p")
    if [ -n "$WSL_DISTRO_NAME" ] && command -v wslpath >/dev/null 2>&1; then
        wslpath -w "$p"
    elif command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$p"
    else
        printf '%s' "$p"
    fi
}

embterm_find_file() {
    local f
    for f in "$@"; do
        embterm_cmd find-file-other-window "$(embterm_path "$f")"
    done
}

embterm_dired() {
    embterm_cmd dired-other-window "$(embterm_path "${1:-.}")"
}
