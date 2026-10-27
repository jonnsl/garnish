namespace Salle {
    /* A VM in the sidebar, with its state and a start or shut down button */
    [GtkTemplate (ui = "/org/garnish/Salle/widgets/vm-row.ui")]
    public class VmRow : Gtk.ListBoxRow {
        [GtkChild] private unowned Gtk.Box status_dot;
        [GtkChild] private unowned Gtk.Label name_label;
        [GtkChild] private unowned Gtk.Label status_label;
        [GtkChild] private unowned Gtk.Stack action_stack;

        public Vm vm { get; construct; }

        public VmRow (Vm vm) {
            Object (vm: vm);
        }

        construct {
            vm.notify.connect (update);
            update ();
        }

        public override void dispose () {
            vm.notify.disconnect (update);
            dispose_template (typeof (VmRow));
            base.dispose ();
        }

        private void update () {
            name_label.label = vm.display_name;
            status_label.label = vm.describe ();
            foreach (var c in new string[] { "running", "busy", "stopped" }) {
                status_dot.remove_css_class (c);
            }
            status_dot.add_css_class (vm.state.to_css_class ());
            switch (vm.state) {
            case VmState.STOPPED:
                action_stack.visible_child_name = "start";
                break;
            case VmState.RUNNING:
                action_stack.visible_child_name = "shut-down";
                break;
            default:
                action_stack.visible_child_name = "busy";
                break;
            }
            update_property (Gtk.AccessibleProperty.LABEL, "%s, %s".printf (vm.display_name, vm.state.to_label ()), -1);
        }

        [GtkCallback]
        private void on_start_clicked () {
            VmActions.start (this, vm);
        }

        [GtkCallback]
        private void on_shut_down_clicked () {
            VmActions.shut_down (this, vm);
        }
    }
}
