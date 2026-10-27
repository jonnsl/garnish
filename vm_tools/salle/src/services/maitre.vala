namespace Salle {
    /* A VM and what it was given; see vm_tools/maitre/README.md (D-Bus API) */
    public struct VmInfo {
        public string name;
        /* "stopped", "starting", "running" or "stopping" */
        public string state;
        public uint32 cid;
        public string tap;
        public string address;
        public string user;
        public uint32 pid;
        public string log_dir;

        public static VmInfo stopped (string name) {
            return VmInfo () { name = name, state = "stopped", tap = "", address = "", user = "", log_dir = "" };
        }
    }

    [DBus (name = "org.garnish.Maitre1.Error")]
    public errordomain MaitreError {
        [DBus (name = "NoSuchVm")]
        NO_SUCH_VM,
        [DBus (name = "InvalidConfig")]
        INVALID_CONFIG,
        [DBus (name = "AlreadyRunning")]
        ALREADY_RUNNING,
        [DBus (name = "NotRunning")]
        NOT_RUNNING,
        [DBus (name = "NoResources")]
        NO_RESOURCES,
        [DBus (name = "StartFailed")]
        START_FAILED,
        [DBus (name = "ShuttingDown")]
        SHUTTING_DOWN,
    }

    [DBus (name = "org.garnish.Maitre1")]
    private interface MaitreBus : Object {
        /* Returns once crosvm runs, which can take a while with a slow disk */
        [DBus (timeout = 120000)]
        public abstract async VmInfo start_vm (string name) throws Error;
        public abstract async void stop_vm (string name) throws Error;
        public abstract async void kill_vm (string name) throws Error;
        public abstract async VmInfo[] list_vms () throws Error;

        public signal void vm_started (VmInfo info);
        public signal void vm_stopped (string name, string reason);
    }

    /**
     * The VMs, as maitred and the configuration directory see them.
     *
     * maitred is started by D-Bus activation on the first call. Without it
     * the VMs are listed from their configuration files, so they can still
     * be created and edited.
     */
    public class Maitre : Object {
        public const string BUS_NAME = "org.garnish.Maitre";
        public const string OBJECT_PATH = "/org/garnish/Maitre";

        private static Maitre? instance;

        private MaitreBus? bus;
        private FileMonitor? monitor;
        private uint refresh_source;

        /* Every configured VM, by name */
        public ListStore vms { get; private set; }

        /* Whether the last call to maitred got through */
        public bool available { get; private set; }

        /* Why maitred can't be reached, for people */
        public string? error_message { get; private set; }

        /* Emitted when a VM stops, with maitred's description of why */
        public signal void vm_stopped (Vm vm, string reason);

        /* Emitted when a VM's configuration files change */
        public signal void config_changed (string name);

        public static Maitre get_default () {
            if (instance == null) {
                instance = new Maitre ();
            }
            return instance;
        }

        construct {
            vms = new ListStore (typeof (Vm));
            var dir = File.new_for_path (VmConfig.vm_dir ());
            try {
                DirUtils.create_with_parents (dir.get_path (), 0755);
                monitor = dir.monitor_directory (FileMonitorFlags.WATCH_MOVES, null);
                monitor.changed.connect (on_dir_changed);
            } catch (Error e) {
                warning ("Can't watch %s: %s", dir.get_path (), e.message);
            }
            // maitred stopping takes its VMs along
            Bus.watch_name (BusType.SESSION, BUS_NAME, BusNameWatcherFlags.NONE,
                            () => refresh.begin (),
                            () => {
                for (uint i = 0; i < vms.get_n_items (); i++) {
                    ((Vm) vms.get_item (i)).set_stopped ();
                }
            });
        }

        public Vm? find (string name) {
            for (uint i = 0; i < vms.get_n_items (); i++) {
                var vm = (Vm) vms.get_item (i);
                if (vm.name == name) {
                    return vm;
                }
            }
            return null;
        }

        private async MaitreBus get_bus () throws Error {
            if (bus == null) {
                bus = yield Bus.get_proxy<MaitreBus> (BusType.SESSION, BUS_NAME, OBJECT_PATH,
                                                     DBusProxyFlags.DO_NOT_LOAD_PROPERTIES
                                                     | DBusProxyFlags.DO_NOT_AUTO_START_AT_CONSTRUCTION);
                bus.vm_started.connect ((info) => {
                    var vm = find (info.name);
                    if (vm != null) {
                        vm.update (info);
                    } else {
                        refresh.begin ();
                    }
                });
                bus.vm_stopped.connect ((name, reason) => {
                    var vm = find (name);
                    if (vm != null) {
                        vm.set_stopped ();
                        vm_stopped (vm, reason);
                    }
                    // Its configuration may have gone while it ran
                    refresh.begin ();
                });
            }
            return bus;
        }

        /* Updates the list from maitred, or from the configuration files without it */
        public async void refresh () {
            VmInfo[] infos;
            try {
                var b = yield get_bus ();
                infos = yield b.list_vms ();
                available = true;
                error_message = null;
            } catch (Error e) {
                available = false;
                error_message = describe_error (e);
                infos = list_configs ();
            }
            merge (infos);
        }

        /* What maitred would list, stopped */
        private VmInfo[] list_configs () {
            VmInfo[] infos = {};
            try {
                var dir = Dir.open (VmConfig.vm_dir ());
                string? file;
                while ((file = dir.read_name ()) != null) {
                    if (!file.has_suffix (".conf")) {
                        continue;
                    }
                    var name = file.substring (0, file.length - 5);
                    var path = Path.build_filename (VmConfig.vm_dir (), file);
                    if (VmConfig.is_valid_name (name) && FileUtils.test (path, FileTest.IS_REGULAR)) {
                        infos += VmInfo.stopped (name);
                    }
                }
            } catch (FileError e) {
                // No VMs yet
            }
            return infos;
        }

        /* Keeps the Vm objects of VMs that are still there, sorted by name */
        private void merge (VmInfo[] infos) {
            var seen = new GenericSet<string> (str_hash, str_equal);
            foreach (var info in infos) {
                seen.add (info.name);
                var vm = find (info.name);
                if (vm == null) {
                    vm = new Vm (info.name);
                    vms.insert_sorted (vm, (a, b) => strcmp (((Vm) a).name, ((Vm) b).name));
                }
                vm.update (info);
            }
            for (var i = (int) vms.get_n_items () - 1; i >= 0; i--) {
                if (!seen.contains (((Vm) vms.get_item (i)).name)) {
                    vms.remove (i);
                }
            }
        }

        private void on_dir_changed (File file, File? other, FileMonitorEvent event) {
            if (event == FileMonitorEvent.CHANGES_DONE_HINT || event == FileMonitorEvent.ATTRIBUTE_CHANGED) {
                return;
            }
            foreach (var f in new File?[] { file, other }) {
                if (f == null) {
                    continue;
                }
                var name = f.get_basename ();
                if (name.has_suffix (".conf.d")) {
                    name = name.substring (0, name.length - 7);
                } else if (name.has_suffix (".conf")) {
                    name = name.substring (0, name.length - 5);
                } else {
                    continue;
                }
                config_changed (name);
            }
            // Editors write several events at once
            if (refresh_source != 0) {
                Source.remove (refresh_source);
            }
            refresh_source = Timeout.add (200, () => {
                refresh_source = 0;
                refresh.begin ();
                return Source.REMOVE;
            });
        }

        /* Starts the VM and returns once crosvm runs */
        public async void start (Vm vm) throws Error {
            vm.state = VmState.STARTING;
            try {
                var b = yield get_bus ();
                var info = yield b.start_vm (vm.name);
                vm.update (info);
                available = true;
            } catch (MaitreError e) {
                yield refresh ();
                throw e;
            } catch (Error e) {
                yield refresh ();
                throw new IOError.FAILED ("%s", describe_error (e));
            }
        }

        /* Asks the guest to shut down; maitred stops crosvm if it takes too long */
        public async void shut_down (Vm vm) throws Error {
            yield send (vm, false);
        }

        /* Stops crosvm without waiting for the guest */
        public async void force_off (Vm vm) throws Error {
            yield send (vm, true);
        }

        private async void send (Vm vm, bool kill) throws Error {
            try {
                var b = yield get_bus ();
                if (kill) {
                    yield b.kill_vm (vm.name);
                } else {
                    yield b.stop_vm (vm.name);
                }
                // maitred tells when it's off, not that it's stopping
                vm.state = VmState.STOPPING;
            } catch (MaitreError.NOT_RUNNING e) {
                vm.set_stopped ();
            } catch (Error e) {
                throw new IOError.FAILED ("%s", describe_error (e));
            }
        }

        /* Removes the VM's configuration; the disk image stays */
        public void delete (Vm vm) throws Error {
            FileUtils.unlink (vm.config_path);
            var file = File.new_for_path (vm.config_path);
            if (file.query_exists ()) {
                throw new IOError.FAILED (_("Can’t remove %s"), vm.config_path);
            }
            var pos = 0U;
            if (vms.find (vm, out pos)) {
                vms.remove (pos);
            }
        }

        /* The message of a D-Bus error, without the error name */
        public static string describe_error (Error e) {
            if (e is DBusError.SERVICE_UNKNOWN || e is DBusError.NAME_HAS_NO_OWNER) {
                return _("maitred isn’t installed, or the session bus hasn’t noticed it yet. "
                         + "See the maitre README (Install).");
            }
            if (e is DBusError.SPAWN_CHILD_EXITED || e is DBusError.SPAWN_EXEC_FAILED
                || e is DBusError.SPAWN_FAILED || e is DBusError.SPAWN_SERVICE_NOT_FOUND) {
                return _("maitred didn’t start. See “journalctl --user -u maitred”.");
            }
            if (e is DBusError.NO_REPLY || e is DBusError.TIMED_OUT || e is IOError.TIMED_OUT) {
                return _("maitred didn’t answer in time");
            }
            if (e is IOError.DBUS_ERROR) {
                DBusError.strip_remote_error (e);
            }
            return e.message;
        }
    }
}
