namespace Salle {
    /* Creates a VM, or edits the settings of one */
    [GtkTemplate (ui = "/org/garnish/Salle/dialogs/vm-dialog.ui")]
    public class VmDialog : Adw.Dialog {
        [GtkChild] private unowned Adw.ToastOverlay toast_overlay;
        [GtkChild] private unowned Gtk.Button save_button;
        [GtkChild] private unowned Adw.Banner banner;
        [GtkChild] private unowned Adw.EntryRow name_row;
        [GtkChild] private unowned Adw.ActionRow image_row;
        [GtkChild] private unowned Adw.SpinRow cpus_row;
        [GtkChild] private unowned Adw.SpinRow memory_row;
        [GtkChild] private unowned Adw.SwitchRow network_row;
        [GtkChild] private unowned Adw.SwitchRow wayland_row;
        [GtkChild] private unowned Adw.SwitchRow app_menu_row;
        [GtkChild] private unowned Adw.SwitchRow audio_row;
        [GtkChild] private unowned Adw.SwitchRow microphone_row;
        [GtkChild] private unowned Adw.ActionRow shared_home_row;
        [GtkChild] private unowned Gtk.Button shared_home_clear;
        [GtkChild] private unowned Adw.EntryRow user_row;
        [GtkChild] private unowned Adw.SpinRow uid_row;
        [GtkChild] private unowned Adw.ExpanderRow advanced_row;
        [GtkChild] private unowned Adw.ActionRow kernel_row;
        [GtkChild] private unowned Gtk.Button kernel_reset;
        [GtkChild] private unowned Adw.ActionRow initrd_row;
        [GtkChild] private unowned Gtk.Button initrd_reset;
        [GtkChild] private unowned Adw.EntryRow cmdline_row;
        [GtkChild] private unowned Adw.EntryRow gpu_row;
        [GtkChild] private unowned Adw.EntryRow extra_args_row;

        private Settings settings;
        /* The settings being edited; written when saved */
        private VmConfig config;
        private bool is_new;

        /* Emitted with the VM's name once its settings are written */
        public signal void saved (string name);

        /* Edits `config`, or creates a VM when it's null */
        public VmDialog (VmConfig? config) {
            this.is_new = config == null;
            this.config = config ?? new VmConfig ("");
            load ();
        }

        construct {
            settings = new Settings (Config.APPLICATION_ID);
            cpus_row.adjustment.upper = uint.max (get_num_processors (), 1);
            var host_memory = Utils.host_memory_mib ();
            if (host_memory > 0) {
                // Half a GB at a time, like the spin button
                memory_row.adjustment.upper = (host_memory / 512) / 2.0;
            }
            cpus_row.notify["value"].connect (validate);
            memory_row.notify["value"].connect (validate);
        }

        private void load () {
            title = is_new ? _("New Virtual Machine") : _("Edit “%s”").printf (config.name);
            save_button.label = is_new ? _("_Create") : _("_Save");
            name_row.text = config.name;
            name_row.visible = is_new;
            focus_widget = is_new ? (Gtk.Widget) name_row : null;

            // Settings beyond what this host has still show as they are
            cpus_row.adjustment.upper = double.max (cpus_row.adjustment.upper, config.cpus);
            cpus_row.value = config.cpus;
            memory_row.adjustment.upper = double.max (memory_row.adjustment.upper, config.memory_mib / 1024.0);
            memory_row.value = config.memory_mib / 1024.0;
            network_row.active = config.network;
            wayland_row.active = config.wayland;
            app_menu_row.active = config.app_menu;
            audio_row.active = config.audio;
            microphone_row.active = config.microphone;
            user_row.text = config.user ?? "";
            uid_row.value = config.uid;
            cmdline_row.text = config.cmdline_extra;
            gpu_row.text = config.gpu_context_types;
            extra_args_row.text = config.extra_args;
            advanced_row.expanded = config.kernel != null || config.initrd != null || config.cmdline_extra != ""
                                    || config.gpu_context_types != VmConfig.DEFAULT_GPU_CONTEXT_TYPES
                                    || config.extra_args != "";
            update_files ();
        }

        /* The rows that show paths, and what they default to */
        private void update_files () {
            image_row.subtitle = config.image != null ? Utils.display_path (config.image) : _("None");
            shared_home_row.subtitle = config.shared_home != null
                ? Utils.display_path (config.shared_home)
                : _("None: the guest has its own home");
            shared_home_clear.visible = config.shared_home != null;

            var kernel = config.effective_kernel;
            if (config.kernel != null) {
                kernel_row.subtitle = Utils.display_path (config.kernel);
            } else if (kernel != null) {
                kernel_row.subtitle = _("Next to the image: %s").printf (Path.get_basename (kernel));
            } else {
                kernel_row.subtitle = _("Next to the image");
            }
            kernel_reset.visible = config.kernel != null;

            var initrd = config.effective_initrd;
            if (config.initrd != null) {
                initrd_row.subtitle = Utils.display_path (config.initrd);
            } else if (initrd != null) {
                initrd_row.subtitle = _("Next to the image: %s").printf (Path.get_basename (initrd));
            } else {
                initrd_row.subtitle = _("None");
            }
            initrd_reset.visible = config.initrd != null;
            validate ();
        }

        /* Copies the rows into a config with the name typed in */
        private VmConfig collect () {
            var c = config;
            if (is_new) {
                c = new VmConfig (name_row.text.strip ());
                c.image = config.image;
                c.kernel = config.kernel;
                c.initrd = config.initrd;
                c.shared_home = config.shared_home;
            }
            c.cpus = (int) cpus_row.value;
            c.memory_mib = int64.max ((int64) (memory_row.value * 1024 + 0.5), VmConfig.MIN_MEMORY_MIB);
            c.network = network_row.active;
            c.wayland = wayland_row.active;
            c.app_menu = app_menu_row.active;
            c.audio = audio_row.active;
            c.microphone = audio_row.active && microphone_row.active;
            var user = user_row.text.strip ();
            c.user = user != "" ? user : null;
            c.uid = (int) uid_row.value;
            c.cmdline_extra = cmdline_row.text.strip ();
            var gpu = gpu_row.text.strip ();
            c.gpu_context_types = gpu != "" ? gpu : VmConfig.DEFAULT_GPU_CONTEXT_TYPES;
            c.extra_args = extra_args_row.text.strip ();
            return c;
        }

        /**
         * What keeps the settings from being saved, or null. `unfilled` is
         * set when it's only that the name or image haven't been given yet.
         */
        private string? problem (out bool unfilled) {
            unfilled = false;
            if (is_new) {
                var name = name_row.text.strip ();
                if (name == "") {
                    unfilled = true;
                    return _("Give the virtual machine a name");
                }
                if (!VmConfig.is_valid_name (name)) {
                    return _("Names can have letters, digits, “_”, “.” and “-”, and at most 32 characters");
                }
                if (FileUtils.test (VmConfig.path_for (name), FileTest.EXISTS)) {
                    return _("There already is a virtual machine named “%s”").printf (name);
                }
            }
            unfilled = config.image == null;
            return collect_preview ().problem ();
        }

        /* Like collect, without touching the config being edited */
        private VmConfig collect_preview () {
            var saved_config = config;
            config = config.copy ();
            var preview = collect ();
            config = saved_config;
            return preview;
        }

        [GtkCallback]
        private void on_changed () {
            validate ();
        }

        private void validate () {
            bool unfilled;
            var problem = problem (out unfilled);
            save_button.sensitive = problem == null;
            var name = name_row.text.strip ();
            if (is_new && name != "" && !VmConfig.is_valid_name (name)) {
                name_row.add_css_class ("error");
            } else {
                name_row.remove_css_class ("error");
            }
            // Missing files don't keep it from being saved; they may show up later
            var message = unfilled ? null : (problem ?? config.missing_files ());
            banner.title = message ?? "";
            banner.revealed = message != null;
        }

        [GtkCallback]
        private void on_cancel_clicked () {
            close ();
        }

        [GtkCallback]
        private void on_save_clicked () {
            bool unfilled;
            if (problem (out unfilled) != null) {
                return;
            }
            var c = collect ();
            try {
                c.save ();
            } catch (Error e) {
                toast_overlay.add_toast (new Adw.Toast (_("Can’t save: %s").printf (e.message)));
                return;
            }
            saved (c.name);
            close ();
        }

        [GtkCallback]
        private void on_choose_image_clicked () {
            choose_image.begin ();
        }

        private async void choose_image () {
            var filters = new ListStore (typeof (Gtk.FileFilter));
            var images = new Gtk.FileFilter () { name = _("Disk Images") };
            images.add_suffix ("raw");
            images.add_suffix ("img");
            images.add_suffix ("qcow2");
            filters.append (images);
            var all = new Gtk.FileFilter () { name = _("All Files") };
            all.add_pattern ("*");
            filters.append (all);

            var folder = config.image != null ? Path.get_dirname (config.image) : settings.get_string ("image-folder");
            var dialog = new Gtk.FileDialog () {
                title = _("Choose Disk Image"),
                filters = filters,
                default_filter = images,
                modal = true,
            };
            if (folder != "") {
                dialog.initial_folder = File.new_for_path (folder);
            }
            var file = yield open_file (dialog);
            if (file == null) {
                return;
            }
            config.image = file.get_path ();
            settings.set_string ("image-folder", Path.get_dirname (config.image));
            // mkosi images are named after the distribution, and so are VMs
            if (is_new && name_row.text.strip () == "") {
                var stem = Path.get_basename (VmConfig.replace_extension (config.image, ""));
                if (VmConfig.is_valid_name (stem) && !FileUtils.test (VmConfig.path_for (stem), FileTest.EXISTS)) {
                    name_row.text = stem;
                }
            }
            update_files ();
        }

        [GtkCallback]
        private void on_choose_kernel_clicked () {
            choose_boot_file.begin (true);
        }

        [GtkCallback]
        private void on_choose_initrd_clicked () {
            choose_boot_file.begin (false);
        }

        private async void choose_boot_file (bool kernel) {
            var current = kernel ? config.effective_kernel : config.effective_initrd;
            var folder = current ?? config.image;
            var dialog = new Gtk.FileDialog () {
                title = kernel ? _("Choose Kernel") : _("Choose Initrd"),
                modal = true,
            };
            if (folder != null) {
                dialog.initial_folder = File.new_for_path (Path.get_dirname (folder));
            }
            var file = yield open_file (dialog);
            if (file == null) {
                return;
            }
            if (kernel) {
                config.kernel = file.get_path ();
            } else {
                config.initrd = file.get_path ();
            }
            update_files ();
        }

        [GtkCallback]
        private void on_reset_kernel_clicked () {
            config.kernel = null;
            update_files ();
        }

        [GtkCallback]
        private void on_reset_initrd_clicked () {
            config.initrd = null;
            update_files ();
        }

        [GtkCallback]
        private void on_choose_shared_home_clicked () {
            choose_shared_home.begin ();
        }

        private async void choose_shared_home () {
            var dialog = new Gtk.FileDialog () {
                title = _("Choose Shared Home Folder"),
                modal = true,
                initial_folder = File.new_for_path (config.shared_home ?? Environment.get_home_dir ()),
            };
            try {
                var folder = yield dialog.select_folder (get_root () as Gtk.Window, null);
                config.shared_home = folder.get_path ();
                update_files ();
            } catch (Error e) {
                report_dialog_error (e);
            }
        }

        [GtkCallback]
        private void on_clear_shared_home_clicked () {
            config.shared_home = null;
            update_files ();
        }

        private async File? open_file (Gtk.FileDialog dialog) {
            try {
                var file = yield dialog.open (get_root () as Gtk.Window, null);
                if (file.get_path () == null) {
                    toast_overlay.add_toast (new Adw.Toast (_("Choose a local file")));
                    return null;
                }
                return file;
            } catch (Error e) {
                report_dialog_error (e);
                return null;
            }
        }

        private void report_dialog_error (Error e) {
            if (!(e is Gtk.DialogError.DISMISSED || e is Gtk.DialogError.CANCELLED)) {
                toast_overlay.add_toast (new Adw.Toast (e.message));
            }
        }
    }
}
