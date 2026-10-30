namespace Salle {
    /* A VM's state, how to reach it, and a summary of its settings */
    [GtkTemplate (ui = "/org/garnish/Salle/pages/vm-page.ui")]
    public class VmPage : Adw.NavigationPage {
        [GtkChild] private unowned Gtk.Box status_dot;
        [GtkChild] private unowned Gtk.Label name_label;
        [GtkChild] private unowned Gtk.Label vm_name_label;
        [GtkChild] private unowned Gtk.Label state_label;
        [GtkChild] private unowned Gtk.Button start_button;
        [GtkChild] private unowned Gtk.Button busy_button;
        [GtkChild] private unowned Gtk.Label busy_label;
        [GtkChild] private unowned Gtk.Button shut_down_button;
        [GtkChild] private unowned Gtk.Button force_off_button;
        [GtkChild] private unowned Adw.PreferencesGroup problem_group;
        [GtkChild] private unowned Adw.ActionRow problem_row;
        [GtkChild] private unowned Adw.PreferencesGroup connection_group;
        [GtkChild] private unowned Adw.ActionRow ssh_row;
        [GtkChild] private unowned Gtk.Button terminal_button;
        [GtkChild] private unowned Adw.ActionRow address_row;
        [GtkChild] private unowned Adw.ActionRow tap_row;
        [GtkChild] private unowned Adw.ActionRow cid_row;
        [GtkChild] private unowned Adw.ActionRow pid_row;
        [GtkChild] private unowned Adw.PreferencesGroup sound_group;
        [GtkChild] private unowned Adw.ActionRow volume_row;
        [GtkChild] private unowned Gtk.Scale volume_scale;
        [GtkChild] private unowned Gtk.ToggleButton mute_button;
        [GtkChild] private unowned Adw.PreferencesGroup apps_group;
        [GtkChild] private unowned Adw.ActionRow apps_row;
        [GtkChild] private unowned Adw.PreferencesGroup settings_group;
        [GtkChild] private unowned Adw.ActionRow image_row;
        [GtkChild] private unowned Adw.ActionRow hardware_row;
        [GtkChild] private unowned Adw.ActionRow features_row;
        [GtkChild] private unowned Adw.ActionRow shared_home_row;
        [GtkChild] private unowned Adw.ActionRow user_row;

        public Vm vm { get; construct; }

        private bool audio = false;
        private bool app_menu = false;
        private AppCatalog catalog;
        /* Set while the controls show the mixer's values, so that doesn't
         * count as a change */
        private bool showing_volume = false;

        public VmPage (Vm vm) {
            Object (vm: vm);
        }

        construct {
            terminal_button.visible = VmActions.terminal_available ();
            catalog = new AppCatalog (vm.name);
            catalog.changed.connect (update_apps_row);
            vm.notify.connect (update_state);
            Maitre.get_default ().config_changed.connect (on_config_changed);
            Mixer.get_default ().changed.connect (on_mixer_changed);
            Mixer.get_default ().notify["available"].connect (update_sound_group);
            volume_scale.value_changed.connect (on_volume_changed);
            mute_button.toggled.connect (on_mute_toggled);
            update_state ();
            load_config ();
            update_volume ();
        }

        public override void dispose () {
            vm.notify.disconnect (update_state);
            Maitre.get_default ().config_changed.disconnect (on_config_changed);
            Mixer.get_default ().changed.disconnect (on_mixer_changed);
            Mixer.get_default ().notify["available"].disconnect (update_sound_group);
            catalog.changed.disconnect (update_apps_row);
            catalog.dispose ();
            dispose_template (typeof (VmPage));
            base.dispose ();
        }

        private void on_config_changed (string name) {
            if (name == vm.name) {
                load_config ();
            }
        }

        private void update_state () {
            title = vm.display_name;
            name_label.label = vm.display_name;
            // The name is what maitre and the files go by
            vm_name_label.label = vm.name;
            vm_name_label.visible = vm.title != null;

            var state = vm.state;
            state_label.label = state.to_label ();
            foreach (var c in new string[] { "running", "busy", "stopped" }) {
                status_dot.remove_css_class (c);
            }
            status_dot.add_css_class (state.to_css_class ());

            start_button.visible = state == VmState.STOPPED;
            busy_button.visible = state == VmState.STARTING || state == VmState.BOOTING || state == VmState.STOPPING;
            switch (state) {
            case VmState.STARTING:
                busy_label.label = _("Starting…");
                break;
            case VmState.BOOTING:
                busy_label.label = _("Booting…");
                break;
            default:
                busy_label.label = _("Shutting Down…");
                break;
            }
            shut_down_button.visible = state == VmState.RUNNING;
            force_off_button.visible = state == VmState.BOOTING || state == VmState.RUNNING
                                       || state == VmState.STOPPING;

            connection_group.visible = state == VmState.RUNNING;
            ssh_row.subtitle = "ssh " + vm.ssh_target;
            address_row.visible = vm.address != "";
            address_row.subtitle = vm.address;
            tap_row.visible = vm.tap != "";
            tap_row.subtitle = vm.tap;
            cid_row.subtitle = vm.cid.to_string ();
            pid_row.visible = vm.pid != 0;
            pid_row.subtitle = vm.pid.to_string ();

            update_sound_group ();

            settings_group.description = state != VmState.STOPPED
                ? _("Changes apply the next time the virtual machine starts")
                : null;
        }

        private void load_config () {
            VmConfig config;
            try {
                config = VmConfig.load (vm.name);
            } catch (Error e) {
                // Deleted while shown; the window moves on
                if (!(e is FileError.NOENT)) {
                    show_problem (e.message);
                }
                settings_group.visible = false;
                apps_group.visible = false;
                return;
            }
            settings_group.visible = true;
            image_row.subtitle = config.image != null ? Utils.display_path (config.image) : _("None");
            hardware_row.subtitle = Utils.join_dot ({
                ngettext ("%d Processor", "%d Processors", config.cpus).printf (config.cpus),
                _("%s Memory").printf (Utils.format_memory (config.memory_mib)),
            });
            string[] features = {};
            switch (config.network_mode) {
            case "passt":
                features += _("User-Mode Network");
                break;
            case "nat":
                features += _("NAT Network");
                break;
            case "none":
                break;
            default:
                features += _("Network");
                break;
            }
            if (config.wayland) {
                features += _("Graphical Apps");
            }
            if (config.wayland) {
                switch (config.clipboard) {
                case "to-host":
                    features += _("Copy to Desktop Only");
                    break;
                case "from-host":
                    features += _("Paste from Desktop Only");
                    break;
                case "none":
                    features += _("No Copy and Paste");
                    break;
                default:
                    break;
                }
            }
            if (config.wayland && config.app_menu) {
                features += _("App Menu");
            }
            apps_group.visible = config.wayland;
            app_menu = config.app_menu;
            update_apps_row ();
            if (config.notifications) {
                features += _("Notifications");
            }
            audio = config.audio;
            update_sound_group ();
            if (config.audio) {
                features += config.microphone ? _("Sound and Microphone") : _("Sound");
            }
            features_row.subtitle = features.length > 0 ? Utils.join_dot (features) : _("None");
            shared_home_row.visible = config.shared_home != null;
            shared_home_row.subtitle = config.shared_home != null ? Utils.display_path (config.shared_home) : "";
            user_row.subtitle = "%s (%d)".printf (config.effective_user, config.uid);

            show_problem (config.problem () ?? config.missing_files ());
        }

        private void update_apps_row () {
            var n = catalog.apps.length;
            if (!catalog.listed) {
                apps_row.subtitle = _("Listed once the virtual machine has started");
            } else if (app_menu) {
                apps_row.subtitle = Utils.join_dot ({
                    ngettext ("%u App", "%u Apps", n).printf (n),
                    _("%u in the App Menu").printf (catalog.count_shown ()),
                });
            } else {
                apps_row.subtitle = ngettext ("%u App", "%u Apps", n).printf (n);
            }
        }

        private void update_sound_group () {
            sound_group.visible = audio && vm.state == VmState.RUNNING && Mixer.get_default ().available;
        }

        private void on_mixer_changed (string name) {
            if (name == vm.name) {
                update_volume ();
            }
        }

        private void update_volume () {
            var mixer = Mixer.get_default ();
            var muted = mixer.get_muted (vm.name);
            showing_volume = true;
            volume_scale.set_value (mixer.get_volume (vm.name));
            mute_button.active = muted;
            showing_volume = false;
            mute_button.icon_name = muted ? "audio-volume-muted-symbolic" : "audio-volume-high-symbolic";
            mute_button.tooltip_text = muted ? _("Unmute") : _("Mute");
            volume_scale.sensitive = !muted;
            volume_row.subtitle = mixer.is_pending (vm.name)
                ? _("Applies when the guest next plays sound")
                : "";
            update_sound_group ();
        }

        private void on_volume_changed () {
            if (!showing_volume) {
                Mixer.get_default ().set_volume (vm.name, volume_scale.get_value ());
            }
        }

        private void on_mute_toggled () {
            if (!showing_volume) {
                Mixer.get_default ().set_muted (vm.name, mute_button.active);
            }
        }

        private void show_problem (string? problem) {
            problem_group.visible = problem != null;
            problem_row.subtitle = problem ?? "";
        }

        [GtkCallback]
        private void on_copy_address_clicked () {
            Utils.copy_to_clipboard (this, vm.address);
            VmActions.toast (this, _("IP address copied"));
        }

        [GtkCallback]
        private void on_apps_activated () {
            var nav = get_ancestor (typeof (Adw.NavigationView)) as Adw.NavigationView;
            if (nav != null) {
                nav.push (new AppsPage (vm, catalog));
            }
        }

        [GtkCallback]
        private void on_console_activated () {
            VmActions.show_logs (this, vm, "console");
        }

        [GtkCallback]
        private void on_crosvm_activated () {
            VmActions.show_logs (this, vm, "crosvm");
        }
    }
}
