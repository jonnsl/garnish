namespace Salle {
    public class Application : Adw.Application {
        public Application () {
            Object (
                application_id: Config.APPLICATION_ID,
                resource_base_path: Config.RESOURCE_PATH,
                flags: ApplicationFlags.DEFAULT_FLAGS
            );
        }

        construct {
            ActionEntry[] action_entries = {
                { "about", on_about_action },
                { "quit", quit },
            };
            add_action_entries (action_entries, this);
            set_accels_for_action ("app.quit", { "<primary>q" });
            set_accels_for_action ("win.new-vm", { "<primary>n" });
            set_accels_for_action ("win.refresh", { "F5", "<primary>r" });
            set_accels_for_action ("win.edit", { "<primary>e" });
            set_accels_for_action ("win.logs", { "<primary>l" });
            set_accels_for_action ("win.open-terminal", { "<primary>t" });
        }

        public override void startup () {
            base.startup ();
            // Custom widgets used in templates must be registered before the templates load
            typeof (VmRow).ensure ();
        }

        public override void activate () {
            base.activate ();
            if (active_window == null) {
                var window = new Salle.Window (this);
                // VMs come in one at a time, so the last selection waits for all of them
                var gerant = Gerant.get_default ();
                gerant.refresh.begin ((obj, res) => {
                    gerant.refresh.end (res);
                    window.restore_selection ();
                });
            }
            active_window.present ();
        }

        private void on_about_action () {
            var about = new Adw.AboutDialog () {
                application_name = _("Salle"),
                application_icon = Config.APPLICATION_ID,
                developer_name = "Jonnathan Soares",
                version = Config.VERSION,
                developers = { "Jonnathan Soares" },
                // Translators: Replace "translator-credits" with your name/username, and optionally an email or URL.
                translator_credits = _("translator-credits"),
                copyright = "© 2026 Jonnathan Soares",
                license_type = Gtk.License.GPL_3_0,
                comments = _("A front end for gerantd, which runs crosvm virtual machines."),
            };
            about.present (active_window);
        }
    }
}
