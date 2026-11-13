# EmbTerm のシェル統合（zsh。macOS の既定のシェル）。
#
# EmbTerm の端末の中でだけ読み込む（~/.zshrc の、プロンプトを決めた後で）:
#
#   [[ $TERM_PROGRAM == EmbTerm ]] && source ~/.emacs.d/embterm/etc/embterm.zsh
#   （~/.emacs.d/embterm は Emacs が読む配布物の場所。置き場所を変えていればそこ）
#
# - embterm_cmd 名前 引数...   Emacs の関数を呼ぶ（OSC 51;E。T-118）。呼べるのは embterm.el の
#                              `embterm-shell-commands' に載っているものだけ
# - embterm_find_file パス...  ファイルを Emacs の別のウィンドウで開く（find-file-other-window）
# - embterm_dired [パス]       ディレクトリを Emacs の別のウィンドウで dired で開く（dired-other-window）
#
# bash 版（embterm.bash）との違いは 3 つ（T-131）。
#
# - フックは PROMPT_COMMAND / PS0 ではなく precmd / preexec。`add-zsh-hook' で足すので
#   ほかの設定のフックを潰さない
# - **カレントディレクトリ（OSC 7）もここで出す。** pwsh と bash では利用者のプロンプト側
#   （starship など）が出している前提だが、zsh のプロンプトでこれを出すものが無い。
#   出さないと端末のバッファの `default-directory' が起動したときのまま動かない
# - **precmd はフックの列の先頭に置く。** 2 番目以降だと `$?' が直前のフックの終了コードに
#   なり、OSC 133;D に嘘の値が載る（下のコメント参照）

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
    # :a は存在しないパスでも絶対パスにする（bash 版の `realpath -m' に当たる）。
    # パスの中の symlink は辿らないが、相対パスに補う作業ディレクトリは実体のほう
    # （macOS では /tmp/foo が /private/tmp/foo になる）。どちらも Emacs は開ける
    local p="${1:-.}"
    printf '%s' "${p:a}"
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

# --- カレントディレクトリ（OSC 7）とプロンプトの境界（OSC 133。T-119） ----------
# OSC 7 で `default-directory' が cd に追従し、OSC 133 で前後のプロンプトへ移れる
# （C-c C-p / C-c C-n）、直前の出力をコピーできる（C-c C-o）、終了コードがモードラインに出る。
# PS1 を包むので、starship などでプロンプトを決めた後で読み込む。2 回読み込んでも重ねない。
if [[ -z $__embterm_zsh ]]; then
    __embterm_zsh=1
    __embterm_prompt_count=0

    # file://HOST/PATH。% と空白だけ逃がし、ほかは生のまま送る（pwsh 版と同じ。
    # Emacs 側は `url-unhex-string' を通すので、% を逃がさないと日本語のパスが壊れる）
    __embterm_osc7() {
        local p=${PWD}
        p=${p//\%/%25}
        p=${p// /%20}
        printf '\033]7;file://%s%s\007' "${HOST}" "$p"
    }

    __embterm_precmd() {
        local rc=$?
        if (( __embterm_prompt_count > 0 )); then
            printf '\033]133;D;%s\007' "$rc"
        fi
        (( __embterm_prompt_count++ ))
        __embterm_osc7
        # 【重要】終了コードを戻してから抜ける。zsh は次のフックを「関数呼び出しの後」に
        # 呼ぶので、ここで握り潰すと後ろのフック（starship など）が直前のコマンドの
        # 終了コードを見られなくなる（プロンプトが赤くならない）
        return $rc
    }

    __embterm_preexec() {
        printf '\033]133;C\007'
    }

    autoload -Uz add-zsh-hook
    add-zsh-hook precmd __embterm_precmd
    add-zsh-hook preexec __embterm_preexec
    # 【重要】precmd は列の先頭へ。`add-zsh-hook' は末尾に足すが、2 番目以降のフックが
    # 見る `$?' は**直前のフックの終了コード**なので、そのままだと 133;D に嘘の値が載る
    # （starship を先に読み込んでいると必ずこうなる）。先頭に置き、上の `return $rc' で
    # 後ろのフックには本当の終了コードを渡す
    precmd_functions=(__embterm_precmd ${precmd_functions:#__embterm_precmd})

    # プロンプトの始め（133;A）と入力の始め（133;B）。zsh には PS0 が無いので PS1 に埋める。
    # %{...%} は「幅 0」の印で、これが無いとプロンプトの桁数がずれる
    PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
fi
