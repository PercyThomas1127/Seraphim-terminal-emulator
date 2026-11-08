# Seraphim shell integration for Zsh. Source from ~/.zshrc:
#
#     [[ $TERM_PROGRAM == seraphim ]] && source /path/to/seraphim.zsh
#
# See seraphim.bash for the emitted sequences.

[[ -o interactive ]] || return 0
(( ${+__seraphim_loaded} )) && return 0
typeset -g __seraphim_loaded=1 __seraphim_running=0

__seraphim_urlencode() {
    emulate -L zsh
    setopt extendedglob
    local LC_ALL=C
    print -rn -- ${1//(#b)([^a-zA-Z0-9._~\/-])/%${(l:2::0:)$(( [##16] #match ))}}
}

__seraphim_preexec() {
    __seraphim_running=1
    printf '\e]133;C;cmdline_url=%s\a' "$(__seraphim_urlencode "$1")"
}

__seraphim_precmd() {
    local s=$?
    (( __seraphim_running )) && printf '\e]133;D;%s\a' $s
    __seraphim_running=0
    printf '\e]7;file://%s%s\a' "$HOST" "$(__seraphim_urlencode "$PWD")"
    printf '\e]133;A\a'
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec __seraphim_preexec
add-zsh-hook precmd __seraphim_precmd
PS1="${PS1}%{$(printf '\e]133;B\a')%}"
