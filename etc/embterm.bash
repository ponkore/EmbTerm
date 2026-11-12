# EmbTerm のシェル統合（bash。Git Bash・MSYS2・WSL）。
#
# EmbTerm の端末の中でだけ読み込む（~/.bashrc から）:
#
#   [ "$TERM_PROGRAM" = EmbTerm ] && . ~/.emacs.d/embterm/etc/embterm.bash                 # Git Bash
#   [ "$TERM_PROGRAM" = EmbTerm ] && . /mnt/c/Users/<名前>/.emacs.d/embterm/etc/embterm.bash  # WSL（WSLENV に TERM_PROGRAM を足す）
#   （~/.emacs.d/embterm は Emacs が読む配布物の場所。置き場所を変えていればそこ）
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

# --- プロンプトの境界（OSC 133。T-119） -----------------------------------------
# プロンプトの前に 133;D;終了コード（2 回目から）、PS1 の前後に 133;A / 133;B、コマンドを始めるときに 133;C（PS0。bash 4.4〜）。
# PS1 を決めた後で読み込む。2 回読み込んでも重ねない。
if [ -z "$__embterm_osc133" ]; then
    __embterm_osc133=1
    __embterm_prompt_count=0
    __embterm_precmd() {
        local rc=$?
        if [ "$__embterm_prompt_count" -gt 0 ]; then
            printf '\033]133;D;%s\007' "$rc"
        fi
        __embterm_prompt_count=$((__embterm_prompt_count + 1))
        return $rc
    }
    PROMPT_COMMAND="__embterm_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
    PS0='\e]133;C\a'"$PS0"
fi
