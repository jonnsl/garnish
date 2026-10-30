namespace Salle {
    /* One of a guest's graphical apps */
    public class GuestApp {
        public string id;
        public string name;
        public string? comment;
        public string? icon_file;
        /* Left out of the App Menu */
        public bool hidden;
    }

    /**
     * A VM's apps, as carte last saw them.
     *
     * While the VM runs, `carte sync` keeps the guest's app list in
     * ~/.local/share/carte/apps/<vm>.json, and leaves the apps listed in
     * ~/.config/carte/<vm>.hidden out of the App Menu. Both stay when the VM
     * stops. `carte hide` and `carte show` change the second file and the
     * menu right away. See carte's README.
     */
    public class AppCatalog : Object {
        public string vm_name { get; construct; }
        /* Sorted by name */
        public GenericArray<GuestApp> apps { get; private set; default = new GenericArray<GuestApp> (); }
        /* Whether carte has listed the VM's apps yet */
        public bool listed { get; private set; default = false; }

        private FileMonitor? catalog_monitor;
        private FileMonitor? hidden_monitor;
        /* Changes waiting for carte, by app ID: whether to hide it. They
         * show in `apps` already, so a reload doesn't undo them meanwhile. */
        private HashTable<string, bool> pending = new HashTable<string, bool> (str_hash, str_equal);
        private bool running = false;

        /* Emitted when `apps` or `listed` change */
        public signal void changed ();
        /* Emitted when carte couldn't make a change; `apps` is back to what
         * the files say */
        public signal void failed (string message);

        public AppCatalog (string vm_name) {
            Object (vm_name: vm_name);
        }

        construct {
            catalog_monitor = monitor (catalog_path (vm_name));
            hidden_monitor = monitor (hidden_path (vm_name));
            load ();
        }

        public override void dispose () {
            foreach (var m in new FileMonitor?[] { catalog_monitor, hidden_monitor }) {
                if (m != null) {
                    m.cancel ();
                }
            }
            catalog_monitor = null;
            hidden_monitor = null;
            base.dispose ();
        }

        public static string catalog_path (string vm_name) {
            return Path.build_filename (Environment.get_user_data_dir (), "carte", "apps", vm_name + ".json");
        }

        public static string hidden_path (string vm_name) {
            return Path.build_filename (Environment.get_user_config_dir (), "carte", vm_name + ".hidden");
        }

        public uint count_shown () {
            uint n = 0;
            foreach (var app in apps) {
                if (!app.hidden) {
                    n++;
                }
            }
            return n;
        }

        /* Takes an app out of the App Menu or puts it back, through carte.
         * Changes run one at a time, since each carte call rewrites the
         * hidden file. */
        public void set_hidden (string id, bool hidden) {
            pending[id] = hidden;
            if (!running) {
                run_pending.begin ();
            }
        }

        private async void run_pending () {
            running = true;
            while (pending.size () > 0) {
                var id = pending.get_keys ().data;
                var hidden = pending[id];
                try {
                    yield run_carte (id, hidden);
                } catch (Error e) {
                    // A newer change for it still gets its turn
                    if (pending.contains (id) && pending[id] == hidden) {
                        pending.remove (id);
                    }
                    load ();
                    failed (e.message);
                    continue;
                }
                if (pending.contains (id) && pending[id] == hidden) {
                    pending.remove (id);
                }
            }
            running = false;
            // The monitor would also catch it, a moment later
            load ();
        }

        private async void run_carte (string id, bool hidden) throws Error {
            var proc = new Subprocess.newv ({ "carte", hidden ? "hide" : "show", vm_name, id },
                                            SubprocessFlags.STDOUT_SILENCE | SubprocessFlags.STDERR_PIPE);
            string? stderr_text;
            yield proc.communicate_utf8_async (null, null, null, out stderr_text);
            if (!proc.get_successful ()) {
                var message = (stderr_text ?? "").strip ();
                if (message.has_prefix ("carte: ")) {
                    message = message.substring (7);
                }
                throw new IOError.FAILED (message != "" ? message : _("carte failed"));
            }
        }

        private FileMonitor? monitor (string path) {
            try {
                var m = File.new_for_path (path).monitor_file (FileMonitorFlags.WATCH_MOVES);
                m.changed.connect (on_file_changed);
                return m;
            } catch (Error e) {
                warning ("Can't watch %s: %s", path, e.message);
                return null;
            }
        }

        private void on_file_changed (File file, File? other, FileMonitorEvent event) {
            switch (event) {
            case FileMonitorEvent.CHANGES_DONE_HINT:
            case FileMonitorEvent.DELETED:
            case FileMonitorEvent.CREATED:
            case FileMonitorEvent.MOVED_IN:
            case FileMonitorEvent.MOVED_OUT:
            case FileMonitorEvent.RENAMED:
                load ();
                break;
            default:
                break;
            }
        }

        public void load () {
            string? json = null;
            string hidden_text = "";
            try {
                FileUtils.get_contents (catalog_path (vm_name), out json);
            } catch (FileError e) {
                if (!(e is FileError.NOENT)) {
                    warning ("Can't read %s: %s", catalog_path (vm_name), e.message);
                }
            }
            try {
                FileUtils.get_contents (hidden_path (vm_name), out hidden_text);
            } catch (FileError e) {
                hidden_text = "";
            }
            listed = json != null;
            apps = json != null ? parse (json, hidden_text, Intl.get_language_names ()) : new GenericArray<GuestApp> ();
            foreach (var app in apps) {
                if (pending.contains (app.id)) {
                    app.hidden = pending[app.id];
                }
            }
            changed ();
        }

        /* The apps in carte's catalog, with their names in the first of
         * `languages` the guest has. Anything malformed is skipped: the
         * catalog comes from the guest. */
        public static GenericArray<GuestApp> parse (string json, string hidden_text, string[] languages) {
            var result = new GenericArray<GuestApp> ();
            var hidden = new GenericSet<string> (str_hash, str_equal);
            foreach (var line in hidden_text.split ("\n")) {
                var id = line.strip ();
                if (id != "" && !id.has_prefix ("#")) {
                    hidden.add (id);
                }
            }

            var parser = new Json.Parser ();
            try {
                parser.load_from_data (json);
            } catch (Error e) {
                debug ("Invalid app catalog: %s", e.message);
                return result;
            }
            var root = parser.get_root ();
            if (root == null || root.get_node_type () != Json.NodeType.OBJECT) {
                return result;
            }
            var list = root.get_object ().get_member ("apps");
            if (list == null || list.get_node_type () != Json.NodeType.ARRAY) {
                return result;
            }
            foreach (var node in list.get_array ().get_elements ()) {
                if (node.get_node_type () != Json.NodeType.OBJECT) {
                    continue;
                }
                var obj = node.get_object ();
                var id = get_string (obj, "id");
                var name = get_localized (obj, "name", languages);
                if (id == null || name == null) {
                    continue;
                }
                result.add (new GuestApp () {
                    id = id,
                    name = name,
                    comment = get_localized (obj, "comment", languages),
                    icon_file = get_string (obj, "icon_file"),
                    hidden = hidden.contains (id),
                });
            }
            result.sort ((a, b) => a.name.collate (b.name));
            return result;
        }

        private static string? get_string (Json.Object obj, string key) {
            var node = obj.get_member (key);
            if (node == null || node.get_value_type () != typeof (string)) {
                return null;
            }
            return node.get_string ();
        }

        /* `key` is an object of strings by locale, "" being the default */
        private static string? get_localized (Json.Object obj, string key, string[] languages) {
            var node = obj.get_member (key);
            if (node == null || node.get_node_type () != Json.NodeType.OBJECT) {
                return null;
            }
            var values = node.get_object ();
            foreach (var lang in languages) {
                var value = get_string (values, lang == "C" ? "" : lang);
                if (value != null && value != "") {
                    return value;
                }
            }
            var value = get_string (values, "");
            return value != "" ? value : null;
        }
    }
}
