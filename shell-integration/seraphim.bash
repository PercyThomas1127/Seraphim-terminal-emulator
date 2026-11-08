# Seraphim shell integration for Bash (>= 4.0).
#
# Emits semantic prompt marks (OSC 133) and working-directory reports
# (OSC 7) so Seraphim can publish shell.prompt / command.started /
# command.completed / session.cwd.changed events. Source it from ~/.bashrc:
#
#     [[ $TERM_PROGRAM == seraphim ]] && source /path/to/seraphim.bash
#
# Sequence emitted per command:
#   OSC 133;A            prompt start
#   OSC 133;B            end of prompt / start of user input (in PS1)
#   OSC 133;C;cmdline_url=<percent-encoded command>   command executed
#   OSC 133;D;<status>   command finished

[[ $- == *i* ]] || return 0
[[ -n ${__seraphim_loaded-} ]] && return 0
__seraphim_loaded=1

__seraphim_urlencode() {
    local LC_ALL=C s=$1 out= c i
    for (( i = 0; i < ${#s}; i++ )); do
        c=${s:i:1}
        case $c in
            [a-zA-Z0-9._~/-]) out+=$c ;;
            *) printf -v c '%%%02X' "'$c"; out+=$c ;;
        esac
    done
    printf '%s' "$out"
}

__seraphim_at_prompt=0
__seraphim_running=0

__seraphim_preexec() {
    # Only the first DEBUG trap after the prompt is the user's command.
    [[ $__seraphim_at_prompt == 1 ]] || return 0
    [[ -n ${COMP_LINE-} ]] && return 0
    # An empty command line runs PROMPT_COMMAND directly; that is not a
    # user command.
    [[ $BASH_COMMAND == __seraphim_precmd* ]] && return 0
    __seraphim_at_prompt=0
    __seraphim_running=1
    local cmd
    cmd=$(HISTTIMEFORMAT= builtin history 1)
    cmd=${cmd#*[0-9]  }
    printf '\e]133;C;cmdline_url=%s\a' "$(__seraphim_urlencode "$cmd")"
}

__seraphim_precmd() {
    local status=$?
    __seraphim_at_prompt=0
    if [[ $__seraphim_running == 1 ]]; then
        printf '\e]133;D;%s\a' "$status"
    fi
    __seraphim_running=0
    printf '\e]7;file://%s%s\a' "${HOSTNAME-}" "$(__seraphim_urlencode "$PWD")"
    printf '\e]133;A\a'
    return $status
}

__seraphim_prompt_ready() { __seraphim_at_prompt=1; }

trap '__seraphim_preexec' DEBUG
PROMPT_COMMAND="__seraphim_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}; __seraphim_prompt_ready"
PS1="${PS1}\[\e]133;B\a\]"
