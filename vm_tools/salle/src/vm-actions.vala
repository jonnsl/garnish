namespace Salle.VmActions {
    /* `widget` is where the dialogs and toasts go: any widget in the window */

    public void toast (Gtk.Widget widget, string text) {
        var win = Window.of (widget);
        if (win != null) {
            win.add_toast (new Adw.Toast (text));
        }
    }

    public void start (Gtk.Widget widget, Vm? vm) {
        if (vm == null || vm.state != VmState.STOPPED) {
            return;
        }
        // The row or page may be gone by the time it fails
        var parent = (Gtk.Widget) widget.get_root ();
        Maitre.get_default ().start.begin (vm, (obj, res) => {
            try {
                Maitre.get_default ().start.end (res);
            } catch (Error e) {
                show_start_error (parent, vm, e);
            }
        });
    }

    private void show_start_error (Gtk.Widget widget, Vm vm, Error e) {
        var dialog = new Adw.AlertDialog (_("Can’t Start “%s”").printf (vm.display_name), null);
        // crosvm's own messages come with it, a line each
        var lines = e.message.split ("\n", 2);
        if (lines.length > 1) {
            dialog.body = lines[0];
            var details = new Gtk.Label (lines[1].strip ()) {
                selectable = true,
                wrap = true,
                wrap_mode = Pango.WrapMode.WORD_CHAR,
                xalign = 0,
                css_classes = { "monospace", "caption" },
            };
            dialog.extra_child = new Gtk.ScrolledWindow () {
                child = details,
                hscrollbar_policy = Gtk.PolicyType.NEVER,
                propagate_natural_height = true,
                max_content_height = 240,
            };
        } else {
            dialog.body = e.message;
        }
        dialog.add_response ("close", _("_Close"));
        dialog.add_response ("logs", _("Show _Logs"));
        if (e is MaitreError.INVALID_CONFIG) {
            dialog.add_response ("edit", _("_Edit Settings"));
            dialog.default_response = "edit";
        }
        dialog.close_response = "close";
        dialog.response.connect ((response) => {
            if (response == "logs") {
                show_logs (widget, vm);
            } else if (response == "edit") {
                edit (widget, vm);
            }
        });
        dialog.present (widget);
    }

    public void shut_down (Gtk.Widget widget, Vm? vm) {
        if (vm == null || vm.state != VmState.RUNNING) {
            return;
        }
        var parent = (Gtk.Widget) widget.get_root ();
        var maitre = Maitre.get_default ();
        maitre.shut_down.begin (vm, (obj, res) => {
            try {
                maitre.shut_down.end (res);
            } catch (Error e) {
                toast (parent, _("Can’t shut down %s: %s").printf (vm.display_name, e.message));
            }
        });
    }

    public void force_off (Gtk.Widget widget, Vm? vm) {
        if (vm == null || vm.is_stopped) {
            return;
        }
        // The guest gets no chance to save anything, so ask first
        var dialog = new Adw.AlertDialog (
            _("Force Off “%s”?").printf (vm.display_name),
            _("The virtual machine stops right away, as if its power were cut. Unsaved work in it is lost."));
        dialog.add_response ("cancel", _("_Cancel"));
        dialog.add_response ("force-off", _("_Force Off"));
        dialog.set_response_appearance ("force-off", Adw.ResponseAppearance.DESTRUCTIVE);
        dialog.default_response = "cancel";
        dialog.close_response = "cancel";
        dialog.response.connect ((response) => {
            if (response != "force-off") {
                return;
            }
            var maitre = Maitre.get_default ();
            maitre.force_off.begin (vm, (obj, res) => {
                try {
                    maitre.force_off.end (res);
                } catch (Error e) {
                    toast (widget, _("Can’t force off %s: %s").printf (vm.display_name, e.message));
                }
            });
        });
        dialog.present (widget);
    }

    public void edit (Gtk.Widget widget, Vm? vm) {
        if (vm == null) {
            return;
        }
        VmConfig config;
        try {
            config = VmConfig.load (vm.name);
        } catch (Error e) {
            var alert = new Adw.AlertDialog (_("Can’t Read the Settings of “%s”").printf (vm.display_name), e.message);
            alert.add_response ("close", _("_Close"));
            alert.add_response ("open", _("_Open Configuration File"));
            alert.response.connect ((response) => {
                if (response == "open") {
                    open_config (widget, vm);
                }
            });
            alert.present (widget);
            return;
        }
        var dialog = new VmDialog (config);
        dialog.saved.connect (() => {
            if (!vm.is_stopped) {
                toast (widget, _("Settings saved. They apply the next time %s starts.").printf (vm.display_name));
            }
        });
        dialog.present (widget);
    }

    public void delete (Gtk.Widget widget, Vm? vm) {
        if (vm == null || !vm.is_stopped) {
            return;
        }
        var dialog = new Adw.AlertDialog (
            _("Delete “%s”?").printf (vm.display_name),
            _("Its settings are removed. The disk image and the logs are kept."));
        dialog.add_response ("cancel", _("_Cancel"));
        dialog.add_response ("delete", _("_Delete"));
        dialog.set_response_appearance ("delete", Adw.ResponseAppearance.DESTRUCTIVE);
        dialog.default_response = "cancel";
        dialog.close_response = "cancel";
        dialog.response.connect ((response) => {
            if (response != "delete") {
                return;
            }
            try {
                Maitre.get_default ().delete (vm);
            } catch (Error e) {
                toast (widget, _("Can’t delete %s: %s").printf (vm.display_name, e.message));
            }
        });
        dialog.present (widget);
    }

    public void show_logs (Gtk.Widget widget, Vm? vm, string log = "console") {
        if (vm == null) {
            return;
        }
        new LogDialog (vm, log).present (widget);
    }

    public void copy_ssh (Gtk.Widget widget, Vm? vm) {
        if (vm == null || !vm.is_running) {
            return;
        }
        Utils.copy_to_clipboard (widget, "ssh " + vm.ssh_target);
        toast (widget, _("SSH command copied"));
    }

    /* Terminals are opened through xdg-terminal-exec, the only way that works with any of them */
    public bool terminal_available () {
        return Environment.find_program_in_path ("xdg-terminal-exec") != null;
    }

    public void open_terminal (Gtk.Widget widget, Vm? vm) {
        if (vm == null || !vm.is_running) {
            return;
        }
        try {
            new Subprocess.newv ({ "xdg-terminal-exec", "ssh", vm.ssh_target }, SubprocessFlags.NONE);
        } catch (Error e) {
            toast (widget, _("Can’t open a terminal: %s").printf (e.message));
        }
    }

    public void open_config (Gtk.Widget widget, Vm? vm) {
        if (vm == null) {
            return;
        }
        var launcher = new Gtk.FileLauncher (File.new_for_path (vm.config_path));
        launcher.launch.begin (widget.get_root () as Gtk.Window, null, (obj, res) => {
            try {
                launcher.launch.end (res);
            } catch (Error e) {
                if (!(e is Gtk.DialogError.DISMISSED)) {
                    toast (widget, _("Can’t open %s: %s").printf (vm.config_path, e.message));
                }
            }
        });
    }
}
