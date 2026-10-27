namespace Salle {
    /* A VM's logs, following them while they're written */
    [GtkTemplate (ui = "/org/garnish/Salle/dialogs/log-dialog.ui")]
    public class LogDialog : Adw.Dialog {
        /* Only the end of long logs is shown */
        private const int64 MAX_BYTES = 512 * 1024;

        [GtkChild] private unowned Adw.ToggleGroup log_group;
        [GtkChild] private unowned Gtk.Button open_button;
        [GtkChild] private unowned Gtk.Stack stack;
        [GtkChild] private unowned Gtk.ScrolledWindow scrolled;
        [GtkChild] private unowned Gtk.TextView view;
        [GtkChild] private unowned Adw.StatusPage empty_page;

        private FileMonitor? monitor;
        private uint reload_source;
        private Cancellable? cancellable;
        /* Keeps the end in view as the log grows, until the reader scrolls up */
        private bool follow = true;
        /* Terminal escape sequences (systemd's colors, OSC 3008 context) and control characters */
        private static Regex escapes;

        public Vm vm { get; construct; }

        public LogDialog (Vm vm, string log) {
            Object (vm: vm);
            log_group.active_name = log;
            log_group.notify["active-name"].connect (show_log);
            show_log ();
        }

        static construct {
            try {
                escapes = new Regex ("\x1b\\[[0-9;?]*[ -/]*[@-~]"
                                     + "|\x1b\\][^\x07\x1b]*(\x07|\x1b\\\\)"
                                     + "|\x1b[()][A-Za-z0-9]|\x1b[=>78]"
                                     + "|[\x01-\x08\x0b-\x1f\x7f]");
            } catch (RegexError e) {
                critical ("Bad escape sequence pattern: %s", e.message);
            }
        }

        construct {
            title = _("Logs of %s").printf (vm.name);
            var adj = scrolled.vadjustment;
            adj.changed.connect (() => {
                if (follow) {
                    adj.value = adj.upper - adj.page_size;
                }
            });
            adj.value_changed.connect (() => {
                follow = adj.value >= adj.upper - adj.page_size - 1;
            });
        }

        public override void dispose () {
            stop_following ();
            dispose_template (typeof (LogDialog));
            base.dispose ();
        }

        private string current_path {
            owned get { return vm.log_path (log_group.active_name); }
        }

        private void stop_following () {
            if (monitor != null) {
                monitor.cancel ();
                monitor = null;
            }
            if (reload_source != 0) {
                Source.remove (reload_source);
                reload_source = 0;
            }
            if (cancellable != null) {
                cancellable.cancel ();
                cancellable = null;
            }
        }

        private void show_log () {
            stop_following ();
            view.buffer.text = "";
            follow = true;
            var file = File.new_for_path (current_path);
            try {
                monitor = file.monitor_file (FileMonitorFlags.NONE, null);
                monitor.changed.connect (() => {
                    // Logs grow a line at a time; read them a few times a second at most
                    if (reload_source == 0) {
                        reload_source = Timeout.add (300, () => {
                            reload_source = 0;
                            reload.begin ();
                            return Source.REMOVE;
                        });
                    }
                });
            } catch (Error e) {
                warning ("Can't follow %s: %s", current_path, e.message);
            }
            reload.begin ();
        }

        private async void reload () {
            if (cancellable != null) {
                cancellable.cancel ();
            }
            var c = cancellable = new Cancellable ();
            var file = File.new_for_path (current_path);
            string text;
            try {
                text = yield read_tail (file, c);
            } catch (IOError.CANCELLED e) {
                return;
            } catch (IOError.NOT_FOUND e) {
                show_empty ();
                return;
            } catch (Error e) {
                empty_page.title = _("Can’t Read the Log");
                empty_page.description = e.message;
                stack.visible_child_name = "empty";
                open_button.sensitive = false;
                return;
            }
            if (text == "") {
                show_empty ();
                return;
            }
            stack.visible_child_name = "log";
            open_button.sensitive = true;
            // Replacing the text resets the scroll position
            var was_following = follow;
            view.buffer.text = text;
            follow = was_following;
        }

        private void show_empty () {
            empty_page.title = _("No Log Yet");
            empty_page.description = _("It’s written when the virtual machine starts");
            stack.visible_child_name = "empty";
            open_button.sensitive = false;
        }

        private static async string read_tail (File file, Cancellable c) throws Error {
            var stream = yield file.read_async (Priority.DEFAULT, c);
            var info = yield stream.query_info_async (FileAttribute.STANDARD_SIZE, Priority.DEFAULT, c);
            var size = info.get_size ();
            var skipped = size > MAX_BYTES;
            if (skipped) {
                stream.seek (size - MAX_BYTES, SeekType.SET, c);
            }
            var bytes = yield stream.read_bytes_async ((size_t) int64.min (size, MAX_BYTES), Priority.DEFAULT, c);
            var sb = new StringBuilder.sized (bytes.get_size () + 1);
            sb.append_len ((string) bytes.get_data (), (ssize_t) bytes.get_size ());
            var text = sb.str;
            // Start at a whole line
            if (skipped) {
                var nl = text.index_of_char ('\n');
                text = nl >= 0 ? text.substring (nl + 1) : text;
            }
            text = text.make_valid ();
            return escapes.replace (text, -1, 0, "");
        }

        [GtkCallback]
        private void on_open_clicked () {
            var launcher = new Gtk.FileLauncher (File.new_for_path (current_path));
            launcher.launch.begin (get_root () as Gtk.Window, null, (obj, res) => {
                try {
                    launcher.launch.end (res);
                } catch (Error e) {
                    warning ("Can't open %s: %s", current_path, e.message);
                }
            });
        }

        [GtkCallback]
        private void on_open_folder_clicked () {
            var launcher = new Gtk.FileLauncher (File.new_for_path (vm.log_dir));
            launcher.launch.begin (get_root () as Gtk.Window, null, (obj, res) => {
                try {
                    launcher.launch.end (res);
                } catch (Error e) {
                    warning ("Can't open %s: %s", vm.log_dir, e.message);
                }
            });
        }
    }
}
