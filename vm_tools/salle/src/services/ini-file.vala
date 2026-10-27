namespace Salle {
    public errordomain IniError {
        INVALID,
    }

    /**
     * A systemd/mkosi style configuration file that can be changed in place.
     *
     * Parses like maitre does (vm_tools/maitre/src/ini.rs): `[Section]`
     * headers, `Key=Value` lines, `#` and `;` comments, and values continued
     * on indented lines or after a trailing backslash. Changing a setting
     * rewrites only its own lines, so comments and settings this app doesn't
     * know about stay as they were.
     */
    public class IniFile : Object {
        public class Entry {
            public string section;
            public string key;
            public string value;
            /* The lines the assignment spans, continuations included */
            public int first;
            public int last;
        }

        /* The file the settings are in, for relative paths and messages; may be null */
        public string? path { get; construct; }

        private GenericArray<string> lines = new GenericArray<string> ();
        private GenericArray<Entry> entries = new GenericArray<Entry> ();

        public IniFile (string? path = null) {
            Object (path: path);
        }

        public IniFile.from_data (string text, string? path = null) throws IniError {
            Object (path: path);
            var body = text.has_suffix ("\n") ? text.substring (0, text.length - 1) : text;
            if (text != "") {
                foreach (var line in body.split ("\n")) {
                    lines.add (line);
                }
            }
            parse ();
        }

        public static IniFile load (string path) throws Error {
            string text;
            FileUtils.get_contents (path, out text);
            return new IniFile.from_data (text, path);
        }

        /* The directory relative paths are resolved against */
        public string? dir {
            owned get { return path != null ? Path.get_dirname (path) : null; }
        }

        public string to_data () {
            if (lines.length == 0) {
                return "";
            }
            var sb = new StringBuilder ();
            foreach (var line in lines) {
                sb.append (line);
                sb.append_c ('\n');
            }
            return sb.str;
        }

        /* Writes to a temporary file first, so a failed write leaves the old file */
        public void save () throws Error {
            var data = to_data ();
            File.new_for_path (path).replace_contents (data.data, null, false, FileCreateFlags.NONE, null);
        }

        public unowned GenericArray<Entry> get_entries () {
            return entries;
        }

        public bool has (string section, string key) {
            return find (section, key).length > 0;
        }

        /**
         * Sets a single assignment of `key`: the last one is replaced where it
         * is and earlier ones are removed. With `value` null, every assignment
         * is removed instead.
         */
        public void set_value (string section, string key, string? value) {
            var matches = find (section, key);
            var line = value != null ? "%s=%s".printf (key, value) : null;
            if (matches.length == 0) {
                if (line != null) {
                    insert (section, line);
                }
                return;
            }
            // From the end, so earlier line numbers stay valid
            for (var i = (int) matches.length - 1; i >= 0; i--) {
                var e = matches[i];
                lines.remove_range (e.first, e.last - e.first + 1);
                if (i == (int) matches.length - 1 && line != null) {
                    lines.insert (e.first, line);
                }
            }
            reparse ();
        }

        private GenericArray<Entry> find (string section, string key) {
            var result = new GenericArray<Entry> ();
            foreach (var e in entries) {
                if (e.section == section && e.key == key) {
                    result.add (e);
                }
            }
            return result;
        }

        /* After the section's last setting, or at the end in a new section */
        private void insert (string section, string line) {
            var at = -1;
            foreach (var e in entries) {
                if (e.section == section) {
                    at = e.last + 1;
                }
            }
            if (at < 0) {
                for (var i = 0; i < lines.length; i++) {
                    if (lines[i].strip () == "[%s]".printf (section)) {
                        at = i + 1;
                    }
                }
            }
            if (at < 0) {
                if (lines.length > 0 && lines[lines.length - 1].strip () != "") {
                    lines.add ("");
                }
                lines.add ("[%s]".printf (section));
                at = (int) lines.length;
            }
            lines.insert (at, line);
            reparse ();
        }

        /* The lines only change in ways that keep them valid */
        private void reparse () {
            try {
                parse ();
            } catch (IniError e) {
                critical ("Edited configuration doesn't parse: %s", e.message);
            }
        }

        private void parse () throws IniError {
            entries = new GenericArray<Entry> ();
            string? section = null;
            Entry? last = null;
            // Whether an indented line continues the last entry (mkosi style)
            var continuable = false;
            // Whether the last line ended with a backslash (systemd style)
            var backslash = false;

            for (var i = 0; i < lines.length; i++) {
                var raw = lines[i];
                var trimmed = raw.strip ();
                var ends_with_backslash = trimmed.has_suffix ("\\");
                var content = ends_with_backslash ? trimmed.substring (0, trimmed.length - 1).chomp () : trimmed;
                var indented = raw.length > 0 && raw[0].isspace ();

                if (backslash || (continuable && trimmed != "" && indented)) {
                    if (trimmed.has_prefix ("#") || trimmed.has_prefix (";")) {
                        continue;
                    }
                    if (content != "") {
                        last.value = last.value == "" ? content : "%s %s".printf (last.value, content);
                    }
                    last.last = i;
                    backslash = ends_with_backslash;
                    continue;
                }
                if (trimmed == "") {
                    continuable = false;
                    continue;
                }
                if (trimmed.has_prefix ("#") || trimmed.has_prefix (";")) {
                    continue;
                }
                if (trimmed.has_prefix ("[")) {
                    if (!trimmed.has_suffix ("]")) {
                        throw error (i, _("missing “]”"));
                    }
                    var name = trimmed.substring (1, trimmed.length - 2);
                    if (name == "") {
                        throw error (i, _("empty section name"));
                    }
                    section = name;
                    continuable = false;
                    continue;
                }
                var eq = content.index_of_char ('=');
                if (eq < 0) {
                    throw error (i, _("expected Key=Value, got “%s”").printf (trimmed));
                }
                if (section == null) {
                    throw error (i, _("setting outside of a section"));
                }
                var key = content.substring (0, eq).strip ();
                if (key == "") {
                    throw error (i, _("empty key"));
                }
                last = new Entry () {
                    section = section,
                    key = key,
                    value = content.substring (eq + 1).strip (),
                    first = i,
                    last = i,
                };
                entries.add (last);
                continuable = true;
                backslash = ends_with_backslash;
            }
        }

        private IniError error (int line, string message) {
            return new IniError.INVALID ("%s:%d: %s", path ?? "", line + 1, message);
        }

        /* Systemd's booleans */
        public static bool? parse_bool (string s) {
            switch (s.down ()) {
            case "1":
            case "yes":
            case "y":
            case "true":
            case "t":
            case "on":
                return true;
            case "0":
            case "no":
            case "n":
            case "false":
            case "f":
            case "off":
                return false;
            default:
                return null;
            }
        }

        /**
         * Splits a value into words on whitespace. Single and double quotes
         * group words, and a backslash escapes the next character.
         */
        public static string[] split_words (string s) throws IniError {
            string[] words = {};
            var word = new StringBuilder ();
            var in_word = false;
            unichar quote = 0;
            var escaped = false;
            int i = 0;
            unichar c;
            while (s.get_next_char (ref i, out c)) {
                if (escaped) {
                    word.append_unichar (c);
                    escaped = false;
                } else if (c == '\\') {
                    escaped = true;
                    in_word = true;
                } else if (quote != 0 && c == quote) {
                    quote = 0;
                } else if (quote != 0) {
                    word.append_unichar (c);
                } else if (c == '"' || c == '\'') {
                    quote = c;
                    in_word = true;
                } else if (c.isspace ()) {
                    if (in_word) {
                        words += word.str;
                        word.truncate ();
                        in_word = false;
                    }
                } else {
                    word.append_unichar (c);
                    in_word = true;
                }
            }
            if (escaped) {
                throw new IniError.INVALID (_("trailing backslash"));
            }
            if (quote != 0) {
                throw new IniError.INVALID (_("unterminated quote"));
            }
            if (in_word) {
                words += word.str;
            }
            return words;
        }

        /**
         * A size in bytes, with an optional K, M, G or T suffix (powers of
         * 1024, optionally followed by "iB" or "B"); -1 if it isn't one.
         */
        public static int64 parse_size (string s) {
            var t = s.strip ();
            var digits = 0;
            while (digits < t.length && t[digits].isdigit ()) {
                digits++;
            }
            uint64 num = 0;
            if (digits == 0 || !uint64.try_parse (t.substring (0, digits), out num)) {
                return -1;
            }
            var suffix = t.substring (digits).strip ();
            if (suffix.has_suffix ("iB")) {
                suffix = suffix.substring (0, suffix.length - 2);
            } else if (suffix.has_suffix ("B")) {
                suffix = suffix.substring (0, suffix.length - 1);
            }
            int shift;
            switch (suffix.up ()) {
            case "":
                shift = 0;
                break;
            case "K":
                shift = 10;
                break;
            case "M":
                shift = 20;
                break;
            case "G":
                shift = 30;
                break;
            case "T":
                shift = 40;
                break;
            default:
                return -1;
            }
            if (num > (int64.MAX >> shift)) {
                return -1;
            }
            return (int64) (num << shift);
        }

        /* Expands a leading "~/" and makes `value` absolute relative to `dir` */
        public static string resolve_path (string value, string? dir) {
            var home = Environment.get_home_dir ();
            if (value == "~") {
                return home;
            }
            if (value.has_prefix ("~/")) {
                return Path.build_filename (home, value.substring (2));
            }
            if (Path.is_absolute (value) || dir == null) {
                return value;
            }
            return Path.build_filename (dir, value);
        }

        /* How a path is written: under the home directory with "~/" */
        public static string format_path (string path) {
            var home = Environment.get_home_dir ();
            if (path.has_prefix (home + "/")) {
                return "~/" + path.substring (home.length + 1);
            }
            return path;
        }
    }
}
