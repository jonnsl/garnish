# bash completion for carte                               -*- shell-script -*-

# The VMs with launchers: ~/.local/share/applications/carte-<vm>.<id>.desktop
_carte_vms()
{
    local dir=${XDG_DATA_HOME:-$HOME/.local/share}/applications f
    for f in "$dir"/carte-*.*.desktop; do
        [[ -f $f ]] || continue
        f=${f##*/carte-}
        printf '%s\n' "${f%%.*}"
    done | sort -u
}

# The applications of a VM, by ID
_carte_apps()
{
    local dir=${XDG_DATA_HOME:-$HOME/.local/share}/applications f
    for f in "$dir"/carte-"$1".*.desktop; do
        [[ -f $f ]] || continue
        f=${f##*/carte-"$1".}
        printf '%s\n' "${f%.desktop}"
    done
}

# The applications a VM has taken out of the menu
_carte_hidden()
{
    local f=${XDG_CONFIG_HOME:-$HOME/.config}/carte/$1.hidden
    [[ -f $f ]] && grep -v '^#' "$f"
}

_carte()
{
    local cur prev words cword
    _init_completion || return

    if ((cword == 1)); then
        COMPREPLY=($(compgen -W 'sync launch hide show remove' -- "$cur"))
        return
    fi

    case ${words[1]} in
        sync)
            case $cword in
                2) COMPREPLY=($(compgen -W '--cid' -- "$cur")) ;;
                4) COMPREPLY=($(compgen -W '--no-menu' -- "$cur")) ;;
            esac
            ;;
        launch | hide)
            # launch --no-notify <vm> <id>
            local i=2
            [[ ${words[1]} == launch && ${words[2]} == --no-notify ]] && i=3
            if ((cword == i)); then
                COMPREPLY=($(compgen -W "$(_carte_vms)" -- "$cur"))
                [[ ${words[1]} == launch && $i == 2 ]] && COMPREPLY+=($(compgen -W '--no-notify' -- "$cur"))
            elif ((cword == i + 1)); then
                COMPREPLY=($(compgen -W "$(_carte_apps "${words[i]}")" -- "$cur"))
            fi
            ;;
        show)
            case $cword in
                2) COMPREPLY=($(compgen -W "$(_carte_vms)" -- "$cur")) ;;
                3) COMPREPLY=($(compgen -W "$(_carte_hidden "${words[2]}")" -- "$cur")) ;;
            esac
            ;;
        remove)
            ((cword == 2)) && COMPREPLY=($(compgen -W "$(_carte_vms)" -- "$cur"))
            ;;
    esac
} &&
    complete -F _carte carte

# ex: filetype=sh
