# bash completion for carte                               -*- shell-script -*-

# The VMs with launchers: ~/.local/share/applications/carte-<vm>-<id>.desktop
_carte_vms()
{
    local dir=${XDG_DATA_HOME:-$HOME/.local/share}/applications f
    for f in "$dir"/carte-*-*.desktop; do
        [[ -f $f ]] || continue
        f=${f##*/carte-}
        printf '%s\n' "${f%%-*}"
    done | sort -u
}

# The applications of a VM, by ID
_carte_apps()
{
    local dir=${XDG_DATA_HOME:-$HOME/.local/share}/applications f
    for f in "$dir"/carte-"$1"-*.desktop; do
        [[ -f $f ]] || continue
        f=${f##*/carte-"$1"-}
        printf '%s\n' "${f%.desktop}"
    done
}

_carte()
{
    local cur prev words cword
    _init_completion || return

    if ((cword == 1)); then
        COMPREPLY=($(compgen -W 'sync launch remove' -- "$cur"))
        return
    fi

    case ${words[1]} in
        sync)
            ((cword == 2)) && COMPREPLY=($(compgen -W '--cid' -- "$cur"))
            ;;
        launch)
            case $cword in
                2) COMPREPLY=($(compgen -W "$(_carte_vms)" -- "$cur")) ;;
                3) COMPREPLY=($(compgen -W "$(_carte_apps "${words[2]}")" -- "$cur")) ;;
            esac
            ;;
        remove)
            ((cword == 2)) && COMPREPLY=($(compgen -W "$(_carte_vms)" -- "$cur"))
            ;;
    esac
} &&
    complete -F _carte carte

# ex: filetype=sh
