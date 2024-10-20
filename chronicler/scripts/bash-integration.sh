_chronicler_bin="##CHRONICLER_BIN##"
_chronicler_db=$("$_chronicler_bin" update --show-db-path)

# chronicler session
if [ -f /proc/sys/kernel/random/uuid ]; then
    _chronicler_session=$(< /proc/sys/kernel/random/uuid)
else
    _chronicler_session=$(dd if=/dev/urandom bs=33 count=1 2>/dev/null | base64)
fi

# create pipe bound to fd=42 to send id from `_chronicler_hist_start` to `_chronicler_hist_end`
_chronicler_pipe="/tmp/chronicler-$_chronicler_session.pipe"
mkfifo "$_chronicler_pipe"
exec 42<>"$_chronicler_pipe"
rm -f "$_chronicler_pipe"
unset _chronicler_pipe

# get currently executing command
function _chronicler_curr_cmd() {
    local last_cmd
    last_cmd=$(HISTTIMEFORMAT="" builtin history 1)
    last_cmd="${last_cmd##*([[:space:]])+([[:digit:]])+([[:space:]])}" # remove leading history number and spaces
    builtin printf "%s" "${last_cmd//[[:cntrl:]]}"  # remove any control characters
}

# create entry in the chronicler database
function _chronicler_hist_start {
    local curr_cmd hist_id
    curr_cmd=$(_chronicler_curr_cmd)
    if [[ "$_chronicler_prev_cmd" == "$curr_cmd" ]]; then
        return
    fi
    _chronicler_prev_cmd="$curr_cmd"
    local now="${EPOCHREALTIME:-$(date +%s.01)}"
    local __sep__=$'\x0c'
    hist_id=$("$_chronicler_bin" update <<-EOF
cmd
$curr_cmd
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
$_chronicler_session
EOF
)
    printf "%s\n" "$hist_id" >&42
}
if [[ ! $PS0 =~ "_chronicler_hist_start" ]]; then
    PS0="${PS0}\$(_chronicler_hist_start)"
fi

# update entry in the chronicler database
function _chronicler_hist_end {
    local return_value="$?" # keep this the first command
    local hist_id
    read -r -t 0.01 -u 42 hist_id
    if [[ -z "$hist_id" ]]; then
        return
    fi
    local now=${EPOCHREALTIME:-$(date +%s.01)}
    local __sep__=$'\x0c'
    "$_chronicler_bin" update <<-EOF > /dev/null
id
$hist_id
$__sep__
end_ts
$now
$__sep__
return
$return_value
EOF
}
if [[ ! " ${PROMPT_COMMAND[*]} " =~ ' _chronicler_hist_end ' ]]; then
    PROMPT_COMMAND+=(_chronicler_hist_end)
fi

function _chronicler_readline_extend {
    if [[ -z "$READLINE_LINE" ]]; then
        READLINE_LINE=$1
    else
        READLINE_LINE="${READLINE_LINE}; $1"
    fi
}

function _chronicler_complete {
    READLINE_LINE=""
    IFS=$'\x0c' read -ra items <<< "$1"
    local tag item item_escape mimetype
    for item in "${items[@]}"; do
        tag="${item:0:2}"
        item="${item:2}"
        item_escape=$(printf "%q" "${item}")
        case "$tag" in
            D=) _chronicler_readline_extend "cd $item_escape";;
            F=)
                if [[ ! -e $item ]]; then
                    continue
                fi
                mimetype=$(file --mime-type --dereference --brief "$item")
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
