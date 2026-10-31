namespace Salle {
    /* A VM's graphical apps: clicking one opens it, and in edit mode each
     * has a switch for the App Menu */
    [GtkTemplate (ui = "/org/garnish/Salle/pages/apps-page.ui")]
    public class AppsPage : Adw.NavigationPage {
        [GtkChild] private unowned Gtk.Stack stack;
        [GtkChild] private unowned Adw.PreferencesGroup group;
        [GtkChild] private unowned Gtk.ListBox list;
        [GtkChild] private unowned Adw.StatusPage empty_status;
        [GtkChild] private unowned Gtk.ToggleButton edit_button;

        public Vm vm { get; construct; }
        public AppCatalog catalog { get; construct; }

        private bool app_menu = false;
        /* Set while the switches show the catalog, so that doesn't count as
         * a change */
        private bool syncing = false;
        /* Each row's switch, in edit mode */
        private GenericArray<Gtk.Switch> switches = new GenericArray<Gtk.Switch> ();
        /* What the rows were made from: when only the hidden apps change,
         * the switches are updated and keep their focus */
        private string? rows_key = null;

        public AppsPage (Vm vm, AppCatalog catalog) {
            Object (vm: vm, catalog: catalog);
        }

        construct {
            catalog.changed.connect (update);
            catalog.failed.connect (on_failed);
            Gerant.get_default ().config_changed.connect (on_config_changed);
            vm.notify["state"].connect (update);
            edit_button.toggled.connect (on_edit_toggled);
            load_config ();
        }

        public override void dispose () {
            catalog.changed.disconnect (update);
            catalog.failed.disconnect (on_failed);
            Gerant.get_default ().config_changed.disconnect (on_config_changed);
            vm.notify["state"].disconnect (update);
            // Their handlers hold the page
            switches.length = 0;
            dispose_template (typeof (AppsPage));
            base.dispose ();
        }

        private void on_config_changed (string name) {
            if (name == vm.name) {
                load_config ();
            }
        }

        private void load_config () {
            try {
                app_menu = VmConfig.load (vm.name).app_menu;
            } catch (Error e) {
                app_menu = false;
            }
            edit_button.visible = app_menu;
            if (!app_menu) {
                edit_button.active = false;
            }
            update ();
        }

        private bool editing {
            get { return app_menu && edit_button.active; }
        }

        private bool running {
            get { return vm.state == VmState.RUNNING; }
        }

        private void on_edit_toggled () {
            edit_button.label = edit_button.active ? _("_Done") : _("_Edit");
            if (edit_button.active) {
                edit_button.add_css_class ("suggested-action");
            } else {
                edit_button.remove_css_class ("suggested-action");
            }
            update ();
        }

        private void update () {
            var apps = catalog.apps;
            stack.visible_child_name = apps.length > 0 ? "list" : "empty";
            empty_status.description = catalog.listed
                ? _("The guest has no graphical apps installed")
                : _("They show up here once the virtual machine has started");
            if (editing) {
                group.description = _("Choose which apps are in your App Menu");
            } else if (!running) {
                group.description = _("Start the virtual machine to open its apps");
            } else {
                group.description = null;
            }

            var key = new StringBuilder (editing ? "edit" : running ? "open" : "view");
            foreach (var app in apps) {
                key.append_printf ("\n%s\t%s\t%s\t%s", app.id, app.name, app.comment ?? "", app.icon_file ?? "");
            }
            syncing = true;
            if (key.str != rows_key) {
                rows_key = key.str;
                list.remove_all ();
                switches.length = 0;
                foreach (var app in apps) {
                    list.append (make_row (app));
                }
            } else if (editing) {
                for (var i = 0; i < apps.length; i++) {
                    switches[i].active = !apps[i].hidden;
                }
            }
            syncing = false;
        }

        private Adw.ActionRow make_row (GuestApp app) {
            var row = new Adw.ActionRow ();
            var id = app.id;
            if (editing) {
                // Not an Adw.SwitchRow: when its switch is turned on, it
                // notifies `active` while still reading false. state-set
                // hands over the new state itself.
                var sw = new Gtk.Switch () { active = !app.hidden, valign = Gtk.Align.CENTER };
                sw.update_property (Gtk.AccessibleProperty.LABEL, app.name, -1);
                sw.state_set.connect ((state) => {
                    if (!syncing) {
                        catalog.set_hidden (id, !state);
                    }
                    return false;
                });
                switches.add (sw);
                row.add_suffix (sw);
                row.activatable_widget = sw;
            } else if (running) {
                row.activatable = true;
                var name = app.name;
                row.activated.connect (() => open_app (id, name));
            }
            row.title = Markup.escape_text (app.name);
            row.subtitle = app.comment != null ? Markup.escape_text (app.comment) : "";
            row.subtitle_lines = 2;

            var icon = new Gtk.Image () {
                pixel_size = 32,
                accessible_role = Gtk.AccessibleRole.PRESENTATION,
                css_classes = { "icon-dropshadow" },
            };
            if (app.icon_file != null) {
                icon.gicon = new FileIcon (File.new_for_path (app.icon_file));
            } else {
                icon.icon_name = "application-x-executable-symbolic";
            }
            row.add_prefix (icon);
            return row;
        }

        private void open_app (string id, string name) {
            catalog.launch.begin (id, (obj, res) => {
                try {
                    catalog.launch.end (res);
                } catch (Error e) {
                    VmActions.toast (this, _("Can’t open %s: %s").printf (name, e.message));
                }
            });
        }

        private void on_failed (string message) {
            VmActions.toast (this, _("Can’t change the App Menu: %s").printf (message));
        }
    }
}
