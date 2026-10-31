namespace Salle {
    [GtkTemplate (ui = "/org/garnish/Salle/window.ui")]
    public class Window : Adw.ApplicationWindow {
        [GtkChild] private unowned Adw.ToastOverlay toast_overlay;
        [GtkChild] private unowned Adw.NavigationSplitView split_view;
        [GtkChild] private unowned Adw.Banner unavailable_banner;
        [GtkChild] private unowned Gtk.Stack sidebar_stack;
        [GtkChild] private unowned Gtk.ListBox vm_list;
        [GtkChild] private unowned Adw.NavigationPage placeholder_page;
        [GtkChild] private unowned Adw.StatusPage placeholder_status;
        [GtkChild] private unowned Gtk.Button placeholder_button;

        private Settings settings;
        private Gerant gerant;
        /* The VM whose page is shown */
        private Vm? selected;
        /* Selects the VM from the last session once the list is in */
        private bool restored;

        /* Actions that need a VM in a particular state */
        private const string[] VM_ACTIONS = {
            "start", "shut-down", "force-off", "edit", "delete", "logs", "open-terminal", "copy-ssh", "open-config",
        };

        public Window (Gtk.Application app) {
            Object (application: app);
        }

        construct {
            if (Config.PROFILE == "development") {
                add_css_class ("devel");
            }

            settings = new Settings (Config.APPLICATION_ID);
            settings.bind ("window-width", this, "default-width", SettingsBindFlags.DEFAULT);
            settings.bind ("window-height", this, "default-height", SettingsBindFlags.DEFAULT);
            settings.bind ("window-maximized", this, "maximized", SettingsBindFlags.DEFAULT);

            ActionEntry[] entries = {
                { "new-vm", on_new_vm },
                { "refresh", on_refresh },
                { "start", on_start },
                { "shut-down", on_shut_down },
                { "force-off", on_force_off },
                { "edit", on_edit },
                { "delete", on_delete },
                { "logs", on_show_logs },
                { "open-terminal", on_open_terminal },
                { "copy-ssh", on_copy_ssh },
                { "open-config", on_open_config },
            };
            add_action_entries (entries, this);

            gerant = Gerant.get_default ();
            vm_list.bind_model (gerant.vms, (item) => new VmRow ((Vm) item));
            gerant.vms.items_changed.connect (on_vms_changed);
            gerant.notify["available"].connect (update_banner);
            gerant.notify["error-message"].connect (update_banner);
            gerant.vm_stopped.connect (on_vm_stopped);
            update_banner ();
            on_vms_changed ();

            // Shows the page of a row the user picks, even the selected one
            vm_list.row_activated.connect (() => split_view.show_content = true);
            close_request.connect (() => {
                settings.set_string ("last-selected", selected != null ? selected.name : "");
                return false;
            });
        }

        public override void dispose () {
            gerant.vms.items_changed.disconnect (on_vms_changed);
            gerant.notify["available"].disconnect (update_banner);
            gerant.notify["error-message"].disconnect (update_banner);
            gerant.vm_stopped.disconnect (on_vm_stopped);
            set_selected (null);
            base.dispose ();
        }

        public static Window? of (Gtk.Widget widget) {
            return widget.get_root () as Window;
        }

        public void add_toast (Adw.Toast toast) {
            toast_overlay.add_toast (toast);
        }

        /* Selects a VM; `reveal` also shows its page when only one pane fits */
        public void show_vm (string name, bool reveal = true) {
            var vm = gerant.find (name);
            if (vm == null) {
                return;
            }
            uint pos;
            if (gerant.vms.find (vm, out pos)) {
                vm_list.select_row (vm_list.get_row_at_index ((int) pos));
                if (reveal) {
                    split_view.show_content = true;
                }
            }
        }

        private void update_banner () {
            unavailable_banner.title = gerant.error_message ?? "";
            unavailable_banner.revealed = !gerant.available && gerant.error_message != null;
        }

        private void on_vms_changed () {
            var empty = gerant.vms.get_n_items () == 0;
            sidebar_stack.visible_child_name = empty ? "empty" : "list";
            placeholder_status.title = empty ? _("No Virtual Machines") : _("No Virtual Machine Selected");
            placeholder_status.description = empty ? _("Create one from an existing disk image") : null;
            placeholder_button.visible = empty;

            if (selected != null && gerant.find (selected.name) != selected) {
                vm_list.unselect_all ();
                set_selected (null);
            }
            if (!restored && !empty) {
                restored = true;
                show_vm (settings.get_string ("last-selected"), false);
            }
            // Wide windows always show a VM when there is one
            if (vm_list.get_selected_row () == null && !empty && !split_view.collapsed) {
                vm_list.select_row (vm_list.get_row_at_index (0));
            }
        }

        [GtkCallback]
        private void on_row_selected (Gtk.ListBoxRow? row) {
            set_selected (row != null ? ((VmRow) row).vm : null);
        }

        private void set_selected (Vm? vm) {
            if (vm == selected && vm != null) {
                return;
            }
            if (selected != null) {
                selected.notify["state"].disconnect (update_actions);
            }
            selected = vm;
            if (vm != null) {
                vm.notify["state"].connect (update_actions);
                // A navigation view of its own, for the VM's subpages
                var page = new VmPage (vm);
                var nav = new Adw.NavigationView ();
                nav.add (page);
                var content = new Adw.NavigationPage (nav, page.title) { tag = "vm" };
                page.bind_property ("title", content, "title");
                split_view.content = content;
            } else if (!in_destruction ()) {
                split_view.content = placeholder_page;
                split_view.show_content = false;
            }
            update_actions ();
        }

        private void update_actions () {
            var state = selected != null ? selected.state : VmState.STOPPED;
            foreach (var name in VM_ACTIONS) {
                var enabled = selected != null;
                switch (name) {
                case "start":
                case "delete":
                    enabled = enabled && state == VmState.STOPPED;
                    break;
                case "shut-down":
                case "open-terminal":
                case "copy-ssh":
                    enabled = enabled && state == VmState.RUNNING;
                    break;
                case "force-off":
                    enabled = enabled && (state == VmState.BOOTING || state == VmState.RUNNING
                                          || state == VmState.STOPPING);
                    break;
                default:
                    break;
                }
                ((SimpleAction) lookup_action (name)).set_enabled (enabled);
            }
            ((SimpleAction) lookup_action ("open-terminal")).set_enabled (
                selected != null && state == VmState.RUNNING && VmActions.terminal_available ());
        }

        private void on_vm_stopped (Vm vm, string reason) {
            add_toast (new Adw.Toast (_("%s: %s").printf (vm.display_name, reason)) { timeout = 5 });
        }

        private void on_new_vm () {
            var dialog = new VmDialog (null);
            dialog.saved.connect ((name) => {
                gerant.refresh.begin ((obj, res) => {
                    gerant.refresh.end (res);
                    show_vm (name);
                });
            });
            dialog.present (this);
        }

        private void on_start () {
            VmActions.start (this, selected);
        }

        private void on_shut_down () {
            VmActions.shut_down (this, selected);
        }

        private void on_force_off () {
            VmActions.force_off (this, selected);
        }

        private void on_edit () {
            VmActions.edit (this, selected);
        }

        private void on_delete () {
            VmActions.delete (this, selected);
        }

        private void on_show_logs () {
            VmActions.show_logs (this, selected);
        }

        private void on_open_terminal () {
            VmActions.open_terminal (this, selected);
        }

        private void on_copy_ssh () {
            VmActions.copy_ssh (this, selected);
        }

        private void on_open_config () {
            VmActions.open_config (this, selected);
        }

        private void on_refresh () {
            gerant.refresh.begin ();
        }
    }
}
