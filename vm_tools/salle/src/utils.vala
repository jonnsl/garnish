namespace Salle.Utils {
    public string join_dot (string?[] parts) {
        var b = new StringBuilder ();
        foreach (var p in parts) {
            if (p == null || p == "") {
                continue;
            }
            if (b.len > 0) {
                b.append (" · ");
            }
            b.append (p);
        }
        return b.str;
    }

    /* Memory for people, e.g. "4 GB" or "1.5 GB" */
    public string format_memory (int64 mib) {
        if (mib < 1024) {
            return _("%lld MB").printf (mib);
        }
        if (mib % 1024 == 0) {
            return _("%lld GB").printf (mib / 1024);
        }
        return _("%.1f GB").printf (mib / 1024.0);
    }

    /* The host's memory in MiB, or 0 if it can't be read */
    public int64 host_memory_mib () {
        try {
            string text;
            FileUtils.get_contents ("/proc/meminfo", out text);
            foreach (var line in text.split ("\n")) {
                if (line.has_prefix ("MemTotal:")) {
                    return int64.parse (line.substring (9).strip ().split (" ")[0]) / 1024;
                }
            }
        } catch (FileError e) {
            warning ("Can't read /proc/meminfo: %s", e.message);
        }
        return 0;
    }

    /* A path for people: under the home directory with "~/" */
    public string display_path (string path) {
        return IniFile.format_path (path);
    }

    public void copy_to_clipboard (Gtk.Widget widget, string text) {
        widget.get_clipboard ().set_text (text);
    }
}
