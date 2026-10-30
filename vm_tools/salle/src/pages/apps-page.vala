namespace Salle {
    /* A VM's graphical apps, with a switch each for the App Menu */
    [GtkTemplate (ui = "/org/garnish/Salle/pages/apps-page.ui")]
    public class AppsPage : Adw.NavigationPage {
        [GtkChild] private unowned Gtk.Stack stack;
        [GtkChild] private unowned Adw.PreferencesGroup group;
        [GtkChild] private unowned Gtk.ListBox list;
        [GtkChild] private unowned Adw.StatusPage empty_status;

        public Vm vm { get; construct; }
        public AppCatalog catalog { get; construct; }

        private bool app_menu = false;
        /* Set while the switches show the catalog, so that doesn't count as
         * a change */
        private bool syncing = false;
        /* Each row's switch, with the App Menu on */
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
            Maitre.get_default ().config_changed.connect (on_config_changed);
            load_config ();
        }

        public override void dispose () {
            catalog.changed.disconnect (update);
            catalog.failed.disconnect (on_failed);
            Maitre.get_default ().config_changed.disconnect (on_config_changed);
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
            update ();
        }

        private void update () {
            var apps = catalog.apps;
            stack.visible_child_name = apps.length > 0 ? "list" : "empty";
            empty_status.description = catalog.listed
                ? _("The guest has no graphical apps installed")
                : _("They show up here once the virtual machine has started");
            group.description = app_menu
                ? _("Choose which apps are in your App Menu")
                : _("Turn on “Add Apps to the App Menu” in the settings to launch them from your desktop");

            var key = new StringBuilder (app_menu ? "menu" : "");
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
            } else if (app_menu) {
                for (var i = 0; i < apps.length; i++) {
                    switches[i].active = !apps[i].hidden;
                }
            }
            syncing = false;
        }

        private Adw.ActionRow make_row (GuestApp app) {
            var row = new Adw.ActionRow ();
            if (app_menu) {
                // Not an Adw.SwitchRow: when its switch is turned on, it
                // notifies `active` while still reading false. state-set
                // hands over the new state itself.
                var sw = new Gtk.Switch () { active = !app.hidden, valign = Gtk.Align.CENTER };
                sw.update_property (Gtk.AccessibleProperty.LABEL, app.name, -1);
                var id = app.id;
                sw.state_set.connect ((state) => {
                    if (!syncing) {
                        catalog.set_hidden (id, !state);
                    }
                    return false;
                });
                switches.add (sw);
                row.add_suffix (sw);
                row.activatable_widget = sw;
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

        private void on_failed (string message) {
            VmActions.toast (this, _("Can’t change the App Menu: %s").printf (message));
        }
    }
}
