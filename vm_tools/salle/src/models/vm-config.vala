namespace Salle {
    /**
     * A VM's settings, in `<config dir>/garnish/vms/<name>.conf`.
     *
     * Follows maitre's VmConfig (vm_tools/maitre/src/config.rs). Paths are
     * absolute; null means the default. Settings in drop-ins
     * (`<name>.conf.d/*.conf`) are read but saving only changes the main
     * file, and only the settings that were changed.
     */
    public class VmConfig : Object {
        public const int DEFAULT_CPUS = 4;
        public const int64 DEFAULT_MEMORY_MIB = 4096;
        public const int DEFAULT_UID = 1000;
        public const string DEFAULT_GPU_CONTEXT_TYPES = "cross-domain";
        /* Smallest amount of memory maitre accepts */
        public const int64 MIN_MEMORY_MIB = 128;

        public string name { get; construct; }

        public string? image { get; set; }
        public string? kernel { get; set; }
        public string? initrd { get; set; }
        /* List settings are kept as written, e.g. `quiet "a b"` */
        public string cmdline_extra { get; set; default = ""; }

        public int cpus { get; set; default = DEFAULT_CPUS; }
        public int64 memory_mib { get; set; default = DEFAULT_MEMORY_MIB; }
        public bool network { get; set; default = true; }
        public string? shared_home { get; set; }
        public bool audio { get; set; default = false; }
        public bool microphone { get; set; default = false; }
        public bool wayland { get; set; default = true; }
        public string gpu_context_types { get; set; default = DEFAULT_GPU_CONTEXT_TYPES; }
        public string extra_args { get; set; default = ""; }

        public string? user { get; set; }
        public int uid { get; set; default = DEFAULT_UID; }
        public bool app_menu { get; set; default = true; }

        /* Some settings come from drop-ins, which win over the main file */
        public bool has_dropins { get; private set; }

        /* The settings as they were read, to only write what changed */
        private VmConfig? loaded;

        public VmConfig (string name) {
            Object (name: name);
        }

        public static string vm_dir () {
            return Path.build_filename (Environment.get_user_config_dir (), "garnish", "vms");
        }

        public static string path_for (string name) {
            return Path.build_filename (vm_dir (), name + ".conf");
        }

        public string path {
            owned get { return path_for (name); }
        }

        /* VM names are also file names and socket paths, so short and plain */
        public static bool is_valid_name (string name) {
            return Regex.match_simple ("^[A-Za-z0-9_][A-Za-z0-9_.-]{0,31}$", name);
        }

        public static bool is_valid_user (string user) {
            return Regex.match_simple ("^[A-Za-z0-9_.-]+$", user);
        }

        public static VmConfig load (string name) throws Error {
            var config = new VmConfig (name);
            var files = new GenericArray<IniFile> ();
            files.add (IniFile.load (config.path));
            var dropins = new GenericArray<string> ();
            try {
                var dir = Dir.open (config.path + ".d");
                string? file;
                while ((file = dir.read_name ()) != null) {
                    if (file.has_suffix (".conf")) {
                        dropins.add (Path.build_filename (config.path + ".d", file));
                    }
                }
            } catch (FileError.NOENT e) {
                // No drop-ins
            }
            dropins.sort (strcmp);
            foreach (var dropin in dropins) {
                files.add (IniFile.load (dropin));
            }
            config.has_dropins = dropins.length > 0;
            foreach (var file in files) {
                foreach (var e in file.get_entries ()) {
                    config.apply (e, file.dir);
                }
            }
            config.loaded = config.copy ();
            return config;
        }

        /* An assignment; an empty one resets the setting to its default */
        private void apply (IniFile.Entry e, string? dir) {
            var v = e.value;
            var empty = v == "";
            switch ("%s/%s".printf (e.section, e.key)) {
            case "Boot/Image":
                image = empty ? null : IniFile.resolve_path (v, dir);
                break;
            case "Boot/Kernel":
                kernel = empty ? null : IniFile.resolve_path (v, dir);
                break;
            case "Boot/Initrd":
                initrd = empty ? null : IniFile.resolve_path (v, dir);
                break;
            case "Boot/KernelCommandLineExtra":
                cmdline_extra = append_words (cmdline_extra, v);
                break;
            case "Machine/CPUs":
                int64 n;
                cpus = empty ? DEFAULT_CPUS : (int64.try_parse (v, out n) && n > 0 && n < int.MAX ? (int) n : cpus);
                break;
            case "Machine/Memory":
                var bytes = empty ? DEFAULT_MEMORY_MIB << 20 : IniFile.parse_size (v);
                if (bytes >= 0) {
                    memory_mib = bytes >> 20;
                }
                break;
            case "Machine/Network":
                network = empty || (IniFile.parse_bool (v) ?? network);
                break;
            case "Machine/SharedHome":
                shared_home = empty ? null : IniFile.resolve_path (v, dir);
                break;
            case "Machine/Audio":
                audio = !empty && (IniFile.parse_bool (v) ?? audio);
                break;
            case "Machine/Microphone":
                microphone = !empty && (IniFile.parse_bool (v) ?? microphone);
                break;
            case "Machine/Wayland":
                wayland = empty || (IniFile.parse_bool (v) ?? wayland);
                break;
            case "Machine/GpuContextTypes":
                gpu_context_types = empty ? DEFAULT_GPU_CONTEXT_TYPES : v;
                break;
            case "Machine/ExtraArguments":
                extra_args = append_words (extra_args, v);
                break;
            case "Guest/User":
                user = empty ? null : v;
                break;
            case "Guest/UID":
                int64 n;
                uid = empty ? DEFAULT_UID : (int64.try_parse (v, out n) && n >= 0 && n < int.MAX ? (int) n : uid);
                break;
            case "Guest/AppMenu":
                app_menu = empty || (IniFile.parse_bool (v) ?? app_menu);
                break;
            default:
                // Not edited here (KernelCommandLine=, GID=, ...); maitre checks them
                break;
            }
        }

        /* Lists grow with every assignment, and an empty one clears them */
        private static string append_words (string list, string value) {
            if (value == "") {
                return "";
            }
            return list == "" ? value : "%s %s".printf (list, value);
        }

        public VmConfig copy () {
            var c = new VmConfig (name);
            foreach (var spec in get_class ().list_properties ()) {
                if ((spec.flags & ParamFlags.WRITABLE) != 0 && (spec.flags & ParamFlags.CONSTRUCT_ONLY) == 0
                    && spec.name != "has-dropins") {
                    var value = Value (spec.value_type);
                    get_property (spec.name, ref value);
                    c.set_property (spec.name, value);
                }
            }
            c.has_dropins = has_dropins;
            return c;
        }

        /* The kernel maitre uses: by default the image with .vmlinuz */
        public string? effective_kernel {
            owned get { return kernel ?? (image != null ? replace_extension (image, ".vmlinuz") : null); }
        }

        /* The initrd maitre uses: by default the image with .initrd, if there is one */
        public string? effective_initrd {
            owned get {
                if (initrd != null || image == null) {
                    return initrd;
                }
                var path = replace_extension (image, ".initrd");
                return FileUtils.test (path, FileTest.IS_REGULAR) ? path : null;
            }
        }

        public string effective_user {
            owned get { return user ?? Environment.get_user_name (); }
        }

        /* Like Rust's Path::with_extension */
        public static string replace_extension (string path, string ext) {
            var base_name = Path.get_basename (path);
            var dot = base_name.last_index_of_char ('.');
            var stem = dot > 0 ? path.substring (0, path.length - base_name.length + dot) : path;
            return stem + ext;
        }

        /**
         * What keeps maitre from accepting the settings, or null. Missing
         * files are left to {@link warning}, since they may show up later.
         */
        public string? problem () {
            if (image == null) {
                return _("Choose a disk image");
            }
            if (!is_valid_user (effective_user)) {
                return _("“%s” isn’t a valid user name").printf (effective_user);
            }
            foreach (var list in new string[] { cmdline_extra, gpu_context_types, extra_args }) {
                try {
                    IniFile.split_words (list);
                } catch (IniError e) {
                    return _("“%s”: %s").printf (list, e.message);
                }
            }
            return null;
        }

        /* Files the VM needs that aren't there, or null */
        public string? missing_files () {
            if (image != null && !FileUtils.test (image, FileTest.IS_REGULAR)) {
                return _("The disk image %s doesn’t exist").printf (image);
            }
            var k = effective_kernel;
            if (k != null && !FileUtils.test (k, FileTest.IS_REGULAR)) {
                if (kernel != null) {
                    return _("The kernel %s doesn’t exist").printf (k);
                }
                return _("There’s no kernel next to the image (%s); choose one in Advanced")
                       .printf (Path.get_basename (k));
            }
            if (initrd != null && !FileUtils.test (initrd, FileTest.IS_REGULAR)) {
                return _("The initrd %s doesn’t exist").printf (initrd);
            }
            if (shared_home != null && !FileUtils.test (shared_home, FileTest.IS_DIR)) {
                return _("The shared home folder %s doesn’t exist").printf (shared_home);
            }
            return null;
        }

        /**
         * Writes the settings. A new VM's file gets the main settings; an
         * existing file only has the changed settings rewritten.
         */
        public void save () throws Error {
            var is_new = loaded == null;
            IniFile file;
            if (is_new) {
                DirUtils.create_with_parents (vm_dir (), 0755);
                if (FileUtils.test (path, FileTest.EXISTS)) {
                    throw new IOError.EXISTS (_("There already is a virtual machine named “%s”"), name);
                }
                file = new IniFile.from_data (
                    "# Created by Salle. All settings: see the maitre README (VM configuration).\n", path);
            } else {
                file = IniFile.load (path);
            }
            var old = loaded ?? new VmConfig (name);

            put (file, is_new, "Boot", "Image", path_value (image), image != old.image);
            put (file, false, "Boot", "Kernel", path_value (kernel), kernel != old.kernel);
            put (file, false, "Boot", "Initrd", path_value (initrd), initrd != old.initrd);
            put (file, false, "Boot", "KernelCommandLineExtra", list_value (cmdline_extra),
                 cmdline_extra != old.cmdline_extra);

            put (file, is_new, "Machine", "CPUs", cpus.to_string (), cpus != old.cpus);
            put (file, is_new, "Machine", "Memory", format_memory (memory_mib), memory_mib != old.memory_mib);
            put (file, is_new, "Machine", "Network", bool_value (network), network != old.network);
            put (file, false, "Machine", "SharedHome", path_value (shared_home), shared_home != old.shared_home);
            put (file, is_new, "Machine", "Audio", bool_value (audio), audio != old.audio);
            put (file, is_new, "Machine", "Microphone", bool_value (microphone), microphone != old.microphone);
            put (file, is_new, "Machine", "Wayland", bool_value (wayland), wayland != old.wayland);
            put (file, false, "Machine", "GpuContextTypes",
                 gpu_context_types != DEFAULT_GPU_CONTEXT_TYPES ? gpu_context_types : null,
                 gpu_context_types != old.gpu_context_types);
            put (file, false, "Machine", "ExtraArguments", list_value (extra_args), extra_args != old.extra_args);

            put (file, false, "Guest", "User", user, user != old.user);
            put (file, is_new, "Guest", "UID", uid.to_string (), uid != old.uid);
            put (file, is_new, "Guest", "AppMenu", bool_value (app_menu), app_menu != old.app_menu);

            file.save ();
            loaded = copy ();
        }

        /* `always` writes the setting to a new file even if it's the default */
        private static void put (IniFile file, bool always, string section, string key, string? value, bool changed) {
            if (always || changed) {
                file.set_value (section, key, value);
            }
        }

        private static string? path_value (string? path) {
            return path != null ? IniFile.format_path (path) : null;
        }

        private static string? list_value (string list) {
            return list.strip () != "" ? list.strip () : null;
        }

        private static string bool_value (bool b) {
            return b ? "yes" : "no";
        }

        public static string format_memory (int64 mib) {
            return mib % 1024 == 0 ? "%lldG".printf (mib / 1024) : "%lldM".printf (mib);
        }
    }
}
