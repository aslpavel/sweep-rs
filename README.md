### Sweep
Sweep is a tool used to interactively search through a list of entries. It is inspired by [fzf](https://github.com/junegunn/fzf).
![screenshot](resources/sweep.png)

### Feautres
  - Fast
  - Beautiful
  - Easily custmizable color palette by specifiying only three main colors from which all other colors are drived.
  - JSON-RPC proctol can be used to communicate with sweep process.
  - Includes asyncio [pyton bingins](scripts/sweep.py)
  - Configurable key bindings

### Usage
- **Basic usage**
```
$ sweep --help
Usage: sweep [--height <height>] [-p <prompt>] [--theme <theme>] [--nth <nth>] [-d <delimiter>] [--keep-order] [--scorer <scorer>] [--debug] [--rpc] [--tty <tty>] [--no-match <no-match>] [--title <title>] [--altscreen] [--json] [--io-socket <io-socket>] [--version]

Sweep is a command line fuzzy finder

Options:
  --height          number of lines occupied by sweep
  -p, --prompt      prompt string
  --theme           theme as a list of comma-separated attributes
  --nth             comma-separated list of fields for limiting search scope
  -d, --delimiter   filed delimiter
  --keep-order      keep order (don't use ranking score)
  --scorer          default scorer to rank candidates
  --debug           enable debugging output
  --rpc             use JSON-RPC protocol to communicate
  --tty             path to the TTY
  --no-match        action when there is no match and enter is pressed
  --title           set terminal title
  --altscreen       use alternative screen
  --json            expect candidates in JSON format
  --io-socket       path/descriptor of the unix socket used to communicate
                    instead of stdio/stdin
  --version         show sweep version and quit
  --help            display usage information
```
- **Key bindings**
Current key bindings can be viewed by pressing `ctrc+h` and by default looks like this:

| Name                 | Key Bindings      |
|----------------------|-------------------|
|sweep.scorer.next     | "ctrl+s"          |
|sweep.select          | "ctrl+j" "ctrl+m" |
|sweep.quit            | "ctrl+c" "esc"    |
|sweep.help            | "ctrl+h"          |
|input.move.forward    | "right"           |
|input.move.backward   | "left"            |
|input.move.end        | "ctrl+e"          |
|input.move.start      | "ctrl+a"          |
|input.move.next_word  | "alt+f"           |
|input.move.prev_word  | "alt+b"           |
|input.delete.backward | "backspace"       |
|input.delete.forward  | "delete"          |
|input.delete.end      | "ctrl+k"          |
|list.item.next        | "ctrl+n" "down"   |
|list.item.prev        | "ctrl+p" "up"     |
|input.page.next       | "pagedown"        |
|input.page.prev       | "pageup"          |

- **Bash history integration**
Install sweep and put [`bash_history.py`](scripts/bash_history.py) together with [`sweep.py`](scripts/sweep.py) somewhere in your `$PATH`. Add this to your `~/.bashrc`
```bash
bind '"\er": redraw-current-line'
bind '"\e^": history-expand-line'
bind '"\C-r": " \C-e\C-u\C-y\ey\C-u`bash_history.py`\e\C-e\er\e^"'
```
- **Bash directory history**
Same as with bash history [`path_history.py`](scripts/path_history.py) needs to be located in your `$PATH`. And `~/.bashrc` needs to be extended with.
```bash
__sweep_platform=$(python3 -c 'import sys; print(sys.platform)')

__sweep_path__() {
    path=$(path_history.py select)
    if [ -d "$path" ];  then
        printf 'cd %q' "$path"
    elif [ -f "$path" ]; then
        printf 'cd %q' "$(dirname $path)"
        if (echo "$path" | grep -qE '.*\.(py|rs|h|c|cpp|sh|el|json|js|toml|md|hs|scm|jl|yaml|yml|conf|nix|ini|css|txt|log|diff|patch)$'); then
            $EDITOR "$path"
        elif [[ $(file --mime-type "$path" | awk '{ print $2 }') == text/* ]]; then
            $EDITOR "$path"
        else
            if [ $__sweep_platform = "linux" ]; then
                xdg-open "$path"
            elif [ $__sweep_platform = "darwin" ]; then
                open "$path"
            fi
        fi
    fi
}

__sweep_path_add__() {
    if [ ! "$__sweep_path_prev__" = "$(pwd)" ]; then
        __sweep_path_prev__="$(pwd)"
        path_history.py add
    fi
}
__sweep_path_prev__="$(pwd)"

PROMPT_COMMAND="__sweep_path_add__; $PROMPT_COMMAND"

bind '"\er": redraw-current-line'
bind '"\e^": history-expand-line'
bind '"\C-f": " \C-e\C-u`__sweep_path__`\e\C-e\er\C-m"'
```
`ctrl+f` will open your path history, `tab` will list selected directory, `enter` will open files/directories, `backspace` will list parent directory

- **Sway run command integration**
There is [sweep_kitty.py](scripts/sweep_kitty.py) which creates seprate kitty window. I use it to run commands in sway window manager. It requires [j4-dmenu-desktop](https://github.com/enkore/j4-dmenu-desktop) and [kitty](https://github.com/kovidgoyal/kitty) to be present.
```
set $run_menu j4-dmenu-desktop --no-generic --term=kitty --dmenu='sweep-kitty --no-match=input --theme=dark --prompt="Run"' --no-exec | xargs -r swaymsg -t command exec --
for_window [app_id="kitty" title="sweep-menu"] {
    floating enable
    sticky enable
    resize set width 700 px height 400 px
}

$mod+d exec $run_menu
```
![sway](resources/sway.png)

### Installation
  - Clone
  - Install rust toolchain either with the package manager of your choice or with [rustup](https://rustup.rs/)
  - Build and install it with cargo (default installation path is $HOME/.cargo/bin/sweep make sure it is in your $PATH)
  ```
  $ cargo install --path .
  ```
  - Or build it and copy the binary
  ```
  $ cargo build --release
  $ cp target/release/sweep ~/.bin
  ```
  - Test it
  ```
  $ printf "one\ntwo\nthree" | sweep
  ```
  - Enjoy

### Demo time!
![demo](resources/demo.gif)

### JSON-RPC
- **Wire protocol**
```
<decimal string representing size of JSON object in bytes>\n
<JSON object>
```
- **JSON-RPC [protocol](https://www.jsonrpc.org/specification)**
- **Candidate** - can either be
  - `String` - parsed the same way as lines passed to stdin of the sweep
  - `{"entry": [String|(String, Bool)]}` - JSON object with mandatory `entry` field which is a list of fields, indiviadual fields can either be a tuple with first element being a field value and second indicating whether this field is searchable, or a plain string which is the same as `(<field>, true)`
- **Methods**
  - Extend list of searchable items
    - method: `haystack_extend`
    - params: `[Candidate]`
    - result: `Null`
  - Clear list of searchable items
    - method: `haystack_clear`
    - params: ignored
    - result: `Null`
  - Set query string used to filter items
    - method: `niddle_set`
    - params: `String`
    - result: `Null`
  - Set prompt string (lable string before search input)
    - method: `prompt_set`
    - params: `String`
    - result: `Null`
  - Get currently selected candidate
    - method: `current`
    - params: ingored
    - result: `Candidate`
  - Set key binding
    - method: `key_binding`
    - params: `{"key": String, "tag": String}` - associate keybind key (i.e `ctrl+o`) with the tag, empty tag string means unbind
    - result: `Null`
  - Terminate sweep process
    - method: `terminate`
    - params: ignored
    - result: `Null`
- **Events** (encoded as method calls comming from the sweep process)
  - Entry was selected by pressing `Enter`
    - method: `select`
    - params: `Candidate`
  - Key binding was pressed
    - method: `bind`
    - params: `String` - tag associated with key binding
