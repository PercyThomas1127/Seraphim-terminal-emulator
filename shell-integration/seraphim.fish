# Seraphim shell integration for Fish. Source from config.fish:
#
#     test "$TERM_PROGRAM" = seraphim; and source /path/to/seraphim.fish

status is-interactive; or exit 0
set -q __seraphim_loaded; and exit 0
set -g __seraphim_loaded 1
set -g __seraphim_running 0

function __seraphim_preexec --on-event fish_preexec
    set -g __seraphim_running 1
    printf '\e]133;C;cmdline_url=%s\a' (string escape --style=url -- $argv[1])
end

function __seraphim_postexec --on-event fish_postexec
    set -l s $status
    test $__seraphim_running = 1; and printf '\e]133;D;%s\a' $s
    set -g __seraphim_running 0
end

function __seraphim_prompt --on-event fish_prompt
    printf '\e]7;file://%s%s\a' (hostname) (string escape --style=url -- $PWD)
    printf '\e]133;A\a'
end
