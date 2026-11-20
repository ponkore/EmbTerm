-- EmbTerm のシェル統合（nyagos）。
--
-- EmbTerm の端末の中でだけ読み込む（~/.nyagos などから）:
--
--   if nyagos.env.TERM_PROGRAM == "EmbTerm" then
--       dofile(nyagos.env.USERPROFILE .. "/.emacs.d/embterm/etc/embterm.lua")  -- Emacs が読む配布物の場所
--   end
--
-- - embterm_cmd 名前 引数...   Emacs の関数を呼ぶ（OSC 51;E。T-118）。呼べるのは embterm.el の
--                              `embterm-shell-commands' に載っているものだけ
-- - embterm_find_file パス...  ファイルを Emacs の別のウィンドウで開く（find-file-other-window）
-- - embterm_dired [パス]       ディレクトリを Emacs の別のウィンドウで dired で開く（dired-other-window）
--
-- あわせて、プロンプトの境界（OSC 133。T-119）と、行編集で切り取ったものを Emacs に渡す
-- 設定（Windows のクリップボード経由。#11）を入れる。どちらも下のほうを見ること。
--
-- エスケープは nyagos.write ではなく io.write で出す（加工されない生のバイト列を端末へ流す）。

local function quote(s)
    s = tostring(s):gsub("\\", "\\\\"):gsub('"', '\\"')
    return '"' .. s .. '"'
end

local function embterm_cmd(...)
    local parts = {}
    for _, a in ipairs({ ... }) do
        parts[#parts + 1] = quote(a)
    end
    io.write("\27]51;E" .. table.concat(parts, " ") .. "\27\\")
    io.flush()
end

-- nyagos のカレントディレクトリから絶対パスにする
local function full_path(p)
    p = p or "."
    if not (p:match("^%a:[\\/]") or p:match("^[\\/][\\/]")) then
        p = nyagos.getwd() .. "\\" .. p
    end
    return p
end

-- nyagos の Lua（GopherLua）は 5.1 なので table.unpack が無い
local unpack = table.unpack or unpack

nyagos.alias.embterm_cmd = function(args)
    embterm_cmd(unpack(args))
end

nyagos.alias.embterm_find_file = function(args)
    for _, a in ipairs(args) do
        embterm_cmd("find-file-other-window", full_path(a))
    end
end

nyagos.alias.embterm_dired = function(args)
    embterm_cmd("dired-other-window", full_path(args[1]))
end

-- --- プロンプトの境界（OSC 133。T-119） -----------------------------------------
-- プロンプトの前に 133;D;終了コード（2 回目から）と 133;A、後に 133;B、コマンドを始めるときに 133;C（preexechook）。
-- nyagos.prompt を包むので、starship などでプロンプトを決めた後で読み込む。
if not EMBTERM_OSC133 then
    EMBTERM_OSC133 = true
    local original_prompt = nyagos.prompt
    local count = 0
    nyagos.prompt = function(...)
        local pre = ""
        if count > 0 then
            pre = "\27]133;D;" .. tostring(nyagos.env.ERRORLEVEL or 0) .. "\7"
        end
        count = count + 1
        io.write(pre .. "\27]133;A\7")
        io.flush()
        local r = original_prompt(...)
        io.write("\27]133;B\7")
        io.flush()
        return r
    end
    local original_preexec = nyagos.preexechook
    nyagos.preexechook = function(...)
        -- フックの中の io には flush が無いことがある（nyagos がフックを別の Lua の状態で動かすため）
        io.write("\27]133;C\7")
        if io.flush then
            io.flush()
        end
        if original_preexec then
            return original_preexec(...)
        end
    end
end

-- --- 切り取ったものを Emacs へ（Windows のクリップボード経由。#11） -------------
-- nyagos の切り取り（C-k・C-u・C-w）は go-readline-ny の中のクリップボードに入るだけで、
-- 既定では Emacs から見えない。`clipboard' を立てると行き先が **OS のクリップボード**になり、
-- Emacs 側は C-c C-y（`current-kill' → `interprogram-paste-function'）で拾える。
-- 逆向きも通る（Emacs でコピーしたものが端末の中の C-y で貼れる）。
-- OSC 52 で kill-ring に直に入れる道もあるが、こちらを採った（docs/notes/gh11-ck-cy.md の §6）。
--
-- 【重要】効くのは **rcfile を読んでいる間に立てたときだけ。** nyagos は rcfile を読み終えて
-- から行編集のクリップボードを決める（LazySetup）ので、**起動した後に lua_e などで立てても
-- 効かない。** ~/.nyagos からこのファイルを dofile する形（README の 3.6）なら効く。
-- コマンドラインの --clipboard も同じ。
--
-- 元に戻したいときは、これを読み込んだ後に nyagos.option.clipboard = false と書く。
nyagos.option.clipboard = true
