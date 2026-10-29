using Salle; // vala-lint=using-directive

/* The configuration file editor and VM settings, against a temporary config dir (see tests/meson.build) */

IniFile parse (string text) {
    try {
        return new IniFile.from_data (text, "/cfg/vms/test.conf");
    } catch (IniError e) {
        error (e.message);
    }
}

void write_vm (string name, string text) {
    try {
        DirUtils.create_with_parents (VmConfig.vm_dir (), 0755);
        FileUtils.set_contents (VmConfig.path_for (name), text);
    } catch (Error e) {
        error (e.message);
    }
}

string read_vm (string name) {
    try {
        string text;
        FileUtils.get_contents (VmConfig.path_for (name), out text);
        return text;
    } catch (Error e) {
        error (e.message);
    }
}

VmConfig load_vm (string name) {
    try {
        return VmConfig.load (name);
    } catch (Error e) {
        error (e.message);
    }
}

void save_vm (VmConfig c) {
    try {
        c.save ();
    } catch (Error e) {
        error (e.message);
    }
}

void test_parse () {
    var f = parse ("# comment\n[Boot]\nImage=a.raw\nKernelCommandLineExtra=a \\\n  b\n[Machine]\nCPUs=2\n"
                   + "ExtraArguments=--x\n  --y\n\n  ; not a continuation after a blank line\n");
    var e = f.get_entries ();
    assert (e.length == 4);
    assert (e[0].section == "Boot" && e[0].key == "Image" && e[0].value == "a.raw");
    assert (e[1].value == "a b" && e[1].first == 3 && e[1].last == 4);
    assert (e[3].value == "--x --y" && e[3].last == 8);

    foreach (var bad in new string[] { "Key=1\n", "[Boot\n", "[]\n", "[Boot]\nnovalue\n", "[Boot]\n=1\n" }) {
        try {
            new IniFile.from_data (bad);
            assert_not_reached ();
        } catch (IniError e) {
            // Expected
        }
    }
}

void test_set_value () {
    // Replaced where it is, earlier assignments and continuations removed, comments kept
    var f = parse ("[Machine]\n# CPUs\nCPUs=2\nMemory=4G\nCPUs=3\n  8\n[Guest]\nUID=1000\n");
    f.set_value ("Machine", "CPUs", "6");
    assert (f.to_data () == "[Machine]\n# CPUs\nMemory=4G\nCPUs=6\n[Guest]\nUID=1000\n");

    // New settings go after the section's last one, or in a new section at the end
    f.set_value ("Machine", "Audio", "yes");
    f.set_value ("Boot", "Image", "~/a.raw");
    assert (f.to_data () == "[Machine]\n# CPUs\nMemory=4G\nCPUs=6\nAudio=yes\n[Guest]\nUID=1000\n\n"
            + "[Boot]\nImage=~/a.raw\n");

    f.set_value ("Machine", "Memory", null);
    f.set_value ("Guest", "UID", null);
    f.set_value ("Guest", "User", "me");
    assert (f.to_data () == "[Machine]\n# CPUs\nCPUs=6\nAudio=yes\n[Guest]\nUser=me\n\n[Boot]\nImage=~/a.raw\n");

    var empty = new IniFile ();
    empty.set_value ("Boot", "Image", "x");
    assert (empty.to_data () == "[Boot]\nImage=x\n");
}

void test_helpers () {
    try {
        var w = IniFile.split_words ("a \"b c\" 'd' e\\ f");
        assert (w.length == 4 && w[1] == "b c" && w[2] == "d" && w[3] == "e f");
        IniFile.split_words ("\"open");
        assert_not_reached ();
    } catch (IniError e) {
        // Expected
    }
    assert (IniFile.parse_size ("4G") == 4LL << 30);
    assert (IniFile.parse_size ("512MiB") == 512LL << 20);
    assert (IniFile.parse_size ("1024") == 1024);
    assert (IniFile.parse_size ("1.5G") == -1);
    assert (IniFile.parse_bool ("on") == true && IniFile.parse_bool ("No") == false);
    assert (IniFile.parse_bool ("x") == null);
    assert (IniFile.resolve_path ("../a.raw", "/cfg/vms") == "/cfg/vms/../a.raw");
    assert (IniFile.resolve_path ("~/a.raw", "/cfg") == "/home/me/a.raw");
    assert (IniFile.format_path ("/home/me/a.raw") == "~/a.raw");
    assert (VmConfig.replace_extension ("/x/debian.raw", ".vmlinuz") == "/x/debian.vmlinuz");
    assert (VmConfig.replace_extension ("/x.d/debian", ".vmlinuz") == "/x.d/debian.vmlinuz");
    assert (VmConfig.is_valid_name ("fedora-44-work") && VmConfig.is_valid_name ("a"));
    assert (!VmConfig.is_valid_name ("work_1") && !VmConfig.is_valid_name ("fedora.work"));
    assert (!VmConfig.is_valid_name ("-x") && !VmConfig.is_valid_name ("x-") && !VmConfig.is_valid_name (string.nfill (33, 'x')));
    assert (!VmConfig.is_valid_name (".hidden") && !VmConfig.is_valid_name ("a/b") && !VmConfig.is_valid_name (""));
    assert (VmConfig.format_memory (4096) == "4G" && VmConfig.format_memory (1536) == "1536M");
}

void test_load () {
    write_vm ("load", "[Boot]\nImage=images/debian.raw\nKernelCommandLineExtra=a\nKernelCommandLineExtra=\n"
              + "KernelCommandLineExtra=b \"c d\"\n[Machine]\nTitle=Débian (work)\nCPUs=2\nMemory=8G\n"
              + "Audio=yes\nSharedHome=~/share\n[Network]\nMode=none\n[Guest]\nUser=guest\nGID=5\n");
    DirUtils.create_with_parents (VmConfig.path_for ("load") + ".d", 0755);
    try {
        FileUtils.set_contents (VmConfig.path_for ("load") + ".d/10-more.conf", "[Machine]\nCPUs=3\n");
    } catch (Error e) {
        error (e.message);
    }
    var c = load_vm ("load");
    assert (c.image == Path.build_filename (VmConfig.vm_dir (), "images/debian.raw"));
    assert (c.effective_kernel == Path.build_filename (VmConfig.vm_dir (), "images/debian.vmlinuz"));
    assert (c.cmdline_extra == "b \"c d\"");
    assert (c.title == "Débian (work)" && c.display_name == "Débian (work)");
    assert (c.cpus == 3 && c.memory_mib == 8192 && c.network_mode == "none" && c.audio && !c.microphone && c.wayland);
    assert (c.shared_home == "/home/me/share");
    assert (c.user == "guest" && c.uid == 1000 && c.app_menu);
    assert (c.has_dropins);
    assert (c.problem () == null);
    assert (c.missing_files () != null);
}

void test_save_new () {
    var c = new VmConfig ("new");
    assert (c.problem () != null);
    c.image = "/home/me/images/debian.raw";
    c.memory_mib = 2048;
    assert (c.display_name == "new");
    c.title = "New VM";
    assert (c.problem () == null);
    save_vm (c);
    assert (read_vm ("new") == "# Created by Salle. All settings: see the maitre README (VM configuration).\n\n"
            + "[Boot]\nImage=~/images/debian.raw\n\n"
            + "[Machine]\nTitle=New VM\nCPUs=4\nMemory=2G\nAudio=no\nMicrophone=no\nWayland=yes\n\n"
            + "[Network]\nMode=nat\n\n[Guest]\nUID=1000\nAppMenu=yes\n");

    // Saving a new VM over an existing one fails
    try {
        var dup = new VmConfig ("new");
        dup.image = "/a.raw";
        dup.save ();
        assert_not_reached ();
    } catch (Error e) {
        assert (e is IOError.EXISTS);
    }
}

void test_save_changes () {
    // Only what changed is written; relative paths, comments and unknown settings stay
    write_vm ("edit", "[Boot]\n# the image\nImage=images/debian.raw\nKernelCommandLine=root=/dev/vda\n"
              + "[Machine]\nCPUs=2\n  # an indented comment\nAudio=yes\n");
    var c = load_vm ("edit");
    c.cpus = 8;
    c.audio = false;
    c.shared_home = "/srv/home";
    c.extra_args = "--x";
    c.title = "Work";
    save_vm (c);
    assert (read_vm ("edit") == "[Boot]\n# the image\nImage=images/debian.raw\nKernelCommandLine=root=/dev/vda\n"
            + "[Machine]\nCPUs=8\n  # an indented comment\nAudio=no\nTitle=Work\nSharedHome=/srv/home\n"
            + "ExtraArguments=--x\n");

    // A second save starts from what the first wrote
    c.shared_home = null;
    c.title = null;
    save_vm (c);
    var again = load_vm ("edit");
    assert (again.title == null && !read_vm ("edit").contains ("Title="));
    assert (again.shared_home == null && again.cpus == 8 && again.extra_args == "--x");
}

int main (string[] args) {
    Test.init (ref args);
    Test.add_func ("/ini/parse", test_parse);
    Test.add_func ("/ini/set-value", test_set_value);
    Test.add_func ("/ini/helpers", test_helpers);
    Test.add_func ("/config/load", test_load);
    Test.add_func ("/config/save-new", test_save_new);
    Test.add_func ("/config/save-changes", test_save_changes);
    return Test.run ();
}
