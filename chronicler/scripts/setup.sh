_chronicler_bin="##CHRONICLER_BIN##"
_chronicler_db=$("$_chronicler_bin" update --show-db-path)

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
    local __sep__=$'\x0c'
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
    if [[ -z "$_chronicler_hist_id" ]]; then
        return
    fi
    local now=${EPOCHREALTIME:-$(date +%s.01)}
    local id=$_chronicler_hist_id
    unset _chronicler_hist_id
    local __sep__=$'\x0c'
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

function _chronicler_readline_extend() {
    if [[ -z "$READLINE_LINE" ]]; then
        READLINE_LINE=$1
    else
        READLINE_LINE="${READLINE_LINE}; $1"
    fi
}

function _chronicler_complete() {
    READLINE_LINE=""
    IFS=$'\x0c' read -ra items <<< "$1"
    for item in "${items[@]}"; do
        tag="${item:0:2}"
        item="${item:2}"
        item_escape=$(printf "%q" "${item}")
        case "$tag" in
            D=) _chronicler_readline_extend "cd $item_escape";;
            F=)
                mimetype=$(file --mime-type --brief "$item")
                if [[ $mimetype == text/* || $mimetype == "application/json" ]]; then
                    _chronicler_readline_extend "${EDITOR:-emacs} $item_escape"
                else
                    case "$OSTYPE" in
                        darwin*)
                            # MacOS
                            _chronicler_readline_extend "open $item_escape"
                            ;;
                        linux*|bsd*)
                            # Linux | BSD
                            _chronicler_readline_extend "xdg-open $item_escape"
                            ;;
                        msys*|cygwin*)
                            # Windows
                            ;;
                    esac
                fi
            ;;
            R=)
                _chronicler_readline_extend "$item"
            ;;
        esac
    done
    READLINE_MARK=0
    READLINE_POINT=${#READLINE_LINE}
}

# bind cmd history
function _chronicler_hist_show {
    _chronicler_complete "$("$_chronicler_bin" --query "$READLINE_LINE" cmd)"
}
bind -x '"\C-r": _chronicler_hist_show'

# bind path history
function _chronicler_path_show {
    _chronicler_complete "$("$_chronicler_bin" --query "$READLINE_LINE" path)"
}
bind -x '"\C-f": _chronicler_path_show'
