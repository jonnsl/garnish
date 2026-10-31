# bash completion for gerant                              -*- shell-script -*-

# The configured VMs: ~/.config/garnish/vms/<name>.conf
_gerant_vms()
{
    local dir=${XDG_CONFIG_HOME:-$HOME/.config}/garnish/vms f
    for f in "$dir"/*.conf; do
        [[ -f $f ]] || continue
        f=${f##*/}
        printf '%s\n' "${f%.conf}"
    done
}

_gerant()
{
    local cur prev words cword
    _init_completion || return

    if ((cword == 1)); then
        COMPREPLY=($(compgen -W 'start stop list ssh' -- "$cur"))
        return
    fi

    case ${words[1]} in
        start)
            ((cword == 2)) && COMPREPLY=($(compgen -W "$(_gerant_vms)" -- "$cur"))
            ;;
        stop)
            # gerant stop [--force] <name>, or the name before --force
            local i vm=
            for ((i = 2; i < cword; i++)); do
                [[ ${words[i]} != --force ]] && vm=${words[i]}
            done
            if [[ $cur == -* ]]; then
                [[ " ${words[*]:2:cword-2} " != *" --force "* ]] && COMPREPLY=($(compgen -W '--force' -- "$cur"))
            elif [[ -z $vm ]]; then
                COMPREPLY=($(compgen -W "$(_gerant_vms)" -- "$cur"))
            fi
            ;;
        ssh)
            if ((cword == 2)); then
                COMPREPLY=($(compgen -W "$(_gerant_vms)" -- "$cur"))
            else
                # A command to run in the guest; complete it like a host command
                _command_offset 3
            fi
            ;;
    esac
} &&
    complete -F _gerant gerant

# ex: filetype=sh
