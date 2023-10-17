_chronicler_bin="##CHRONICLER_BIN##"

# chronicler session
if [ -f /proc/sys/kernel/random/uuid ]; then
    _chronicler_hist_session=$(< /proc/sys/kernel/random/uuid)
else
    _chronicler_hist_session=$(dd if=/dev/urandom bs=33 count=1 2>/dev/null | base64)
fi

# pre-exec command
function _chronicler_hist_start {
    local cmd="$1"
    if [[ "$_chronicler_hist_cmd" == "$cmd" ]]; then
        return
    fi
    _chronicler_hist_cmd="$cmd"
    local now="${EPOCHREALTIME:-$(date +%s.01)}"
    local __sep__=$(printf "\x0c")
    _chronicler_hist_id=$("$_chronicler_bin" update <<-EOF
cmd
$cmd
$__sep__
cwd
$PWD
$__sep__
hostname
$HOSTNAME
$__sep__
user
${USER:-$(id -un)}
$__sep__
start_ts
$now
$__sep__
end_ts
$now
$__sep__
session
$_chronicler_hist_session
EOF
)
}
if [[ ! " ${preexec_functions[*]} " =~ " _chronicler_hist_start " ]]; then
    preexec_functions+=(_chronicler_hist_start)
fi

# post-exec command
function _chronicler_hist_end {
    local return_value="$?"
    if [[ ! -n "$_chronicler_hist_id" ]]; then
        return
    fi
    local now=${EPOCHREALTIME:-$(date +%s.01)}
    local id=$_chronicler_hist_id
    unset _chronicler_hist_id
    local __sep__=$(printf "\x0c")
    "$_chronicler_bin" update <<-EOF > /dev/null
id
$id
$__sep__
end_ts
$now
$__sep__
return
$return_value
EOF
}
if [[ ! " ${precmd_functions[*]} " =~ " _chronicler_hist_end " ]]; then
    precmd_functions+=(_chronicler_hist_end)
fi

# bind cmd history
function _chronicler_hist_show {
    READLINE_LINE=$("$_chronicler_bin" --query "$READLINE_LINE" cmd)
    READLINE_MARK=0
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-r": _chronicler_hist_show'

# bind path history
function _chronicler_path_show {
    path=$("$_chronicler_bin" --query "$READLINE_LINE" path)
    path_escape=$(printf "%q" "${path}")
    if [ -d "$path" ];  then
        READLINE_LINE="cd $path_escape"
    elif [ -f "$path" ]; then
        if [[ $(file --mime-type --brief "$path") == text/* ]]; then
            READLINE_LINE="${EDITOR:-emacs} $path_escape"
        else
            case "$OSTYPE" in
                darwin*)
                    # MacOS
                    READLINE_LINE="open $path_escape"
                    ;;
                linux*|bsd*)
                    # Linux | BSD
                    READLINE_LINE="xdg-open $path_escape"
                    ;;
                msys*|cygwin*)
                    # Windows
                    ;;
            esac
        fi
    fi
    READLINE_MARK=0
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-f": _chronicler_path_show'