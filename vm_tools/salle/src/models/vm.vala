namespace Salle {
    public enum VmState {
        STOPPED,
        STARTING,
        /* crosvm runs, the guest hasn't finished booting */
        BOOTING,
        RUNNING,
        STOPPING;

        public static VmState parse (string s) {
            switch (s) {
            case "starting":
                return STARTING;
            case "booting":
                return BOOTING;
            case "running":
                return RUNNING;
            case "stopping":
                return STOPPING;
            default:
                return STOPPED;
            }
        }

        public string to_label () {
            switch (this) {
            case STARTING:
                return _("Starting");
            case BOOTING:
                return _("Booting");
            case RUNNING:
                return _("Running");
            case STOPPING:
                return _("Shutting Down");
            default:
                return _("Stopped");
            }
        }

        /* Style class for the status indicator */
        public string to_css_class () {
            switch (this) {
            case STARTING:
            case BOOTING:
            case STOPPING:
                return "busy";
            case RUNNING:
                return "running";
            default:
                return "stopped";
            }
        }
    }

    /* A configured VM and what maitred gave it while it runs */
    public class Vm : Object {
        public string name { get; construct; }
        public VmState state { get; set; default = VmState.STOPPED; }
        public uint cid { get; set; }
        public string tap { get; set; default = ""; }
        /* The guest's address on its tap */
        public string address { get; set; default = ""; }
        /* The guest user, for ssh */
        public string user { get; set; default = ""; }
        /* crosvm's process ID */
        public uint pid { get; set; }
        /* [Machine] Title= from the configuration */
        public string? title { get; set; }

        /* What people see: the title, or the name without one */
        public string display_name {
            owned get { return title ?? name; }
        }

        public bool is_running {
            get { return state == VmState.RUNNING; }
        }

        public bool is_stopped {
            get { return state == VmState.STOPPED; }
        }

        public Vm (string name) {
            Object (name: name);
        }

        construct {
            notify["state"].connect (() => {
                notify_property ("is-running");
                notify_property ("is-stopped");
            });
            notify["title"].connect (() => notify_property ("display-name"));
        }

        public void update (VmInfo info) {
            freeze_notify ();
            state = VmState.parse (info.state);
            cid = info.cid;
            tap = info.tap;
            address = info.address;
            user = info.user;
            pid = info.pid;
            thaw_notify ();
        }

        public void set_stopped () {
            update (VmInfo.stopped (name));
        }

        /* `ssh <target>` reaches the guest over vsock (systemd's ssh proxy, systemd >= 256) */
        public string ssh_target {
            owned get { return "%s@vsock/%u".printf (user, cid); }
        }

        public string config_path {
            owned get { return VmConfig.path_for (name); }
        }

        /* Where maitred writes the logs; they stay after the VM stops */
        public string log_dir {
            owned get { return Path.build_filename (Environment.get_user_state_dir (), "garnish", name); }
        }

        public string log_path (string log) {
            return Path.build_filename (log_dir, log + ".log");
        }

        /* One line for lists, e.g. "Running · 192.168.10.2" */
        public string describe () {
            if (state == VmState.RUNNING && address != "") {
                return Utils.join_dot ({ state.to_label (), address });
            }
            return state.to_label ();
        }
    }
}
