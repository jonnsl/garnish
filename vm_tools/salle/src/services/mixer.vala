namespace Salle {
    /**
     * Each VM's sound volume, set on the host through PipeWire's PulseAudio
     * server.
     *
     * gerantd names vhost-device-sound's playback stream garnish.<vm>. That
     * stream only exists while the guest has its sound card open, and the
     * guest closes it a few seconds after it goes quiet. When it comes back,
     * WirePlumber gives it the volume it had last; a change made while it's
     * gone is kept here and applied then.
     */
    public class Mixer : Object {
        private const string NODE_PREFIX = "garnish.";
        private const uint RECONNECT_SECONDS = 5;
        /* How long after a stream appears WirePlumber may still restore its
         * saved volume over ours */
        private const int64 RESTORE_USEC = 1000000;

        /* One VM's volume, live or as last known */
        private class Level {
            public uint32 index = PulseAudio.INVALID_INDEX;
            public PulseAudio.CVolume cvolume;
            public double volume = 1.0;
            public bool muted = false;
            /* Changes waiting for the stream to come back */
            public bool volume_pending = false;
            public bool mute_pending = false;
            /* Changes sent and not yet answered; until then, the server's
             * volume is older than ours */
            public int in_flight = 0;
            /* Until then, a change from the server is WirePlumber restoring
             * the saved volume over a change made while the stream was
             * gone, and gets undone */
            public int64 keep_until = 0;
            public bool keep_volume = false;
            public bool keep_mute = false;

            public PulseAudio.Volume pa_volume {
                get { return (PulseAudio.Volume) Math.round (volume * PulseAudio.Volume.NORM); }
            }

            public bool live {
                get { return index != PulseAudio.INVALID_INDEX; }
            }
        }

        private static Mixer? instance;

        private PulseAudio.GLibMainLoop loop;
        private PulseAudio.Context? context;
        private HashTable<string, Level> levels;
        /* Sink input index to VM name, to know which VM a removal is about */
        private HashTable<uint32, string> indexes;
        /* Whose changes were sent, oldest first; the server answers in order */
        private Queue<string> sent = new Queue<string> ();

        /* Whether the sound server can be reached */
        public bool available { get; private set; default = false; }

        /* Emitted when a VM's volume, mute or stream changes */
        public signal void changed (string vm_name);

        public static Mixer get_default () {
            if (instance == null) {
                instance = new Mixer ();
            }
            return instance;
        }

        private Mixer () {
            loop = new PulseAudio.GLibMainLoop ();
            levels = new HashTable<string, Level> (str_hash, str_equal);
            indexes = new HashTable<uint32, string> (direct_hash, direct_equal);
            connect_server ();
        }

        /* Whether the VM's stream exists, so a change applies right away */
        public bool is_live (string vm_name) {
            return get_level (vm_name).live;
        }

        /* Whether a change is waiting for the VM's stream to come back */
        public bool is_pending (string vm_name) {
            var level = get_level (vm_name);
            return level.volume_pending || level.mute_pending;
        }

        /* From 0 to 1, as in GNOME's sound settings */
        public double get_volume (string vm_name) {
            return get_level (vm_name).volume;
        }

        public bool get_muted (string vm_name) {
            return get_level (vm_name).muted;
        }

        public void set_volume (string vm_name, double volume) {
            var level = get_level (vm_name);
            volume = volume.clamp (0, 1);
            if (volume == level.volume && !level.volume_pending) {
                return;
            }
            level.volume = volume;
            level.volume_pending = true;
            apply (level);
            changed (vm_name);
        }

        public void set_muted (string vm_name, bool muted) {
            var level = get_level (vm_name);
            if (muted == level.muted && !level.mute_pending) {
                return;
            }
            level.muted = muted;
            level.mute_pending = true;
            apply (level);
            changed (vm_name);
        }

        private Level get_level (string vm_name) {
            var level = levels[vm_name];
            if (level == null) {
                level = new Level ();
                load_saved (vm_name, level);
                levels[vm_name] = level;
            }
            return level;
        }

        /* Sends the pending changes, if the stream is there */
        private void apply (Level level) {
            if (!level.live || context == null || context.get_state () != PulseAudio.Context.State.READY) {
                return;
            }
            var name = indexes[level.index];
            if (level.volume_pending) {
                var cvolume = level.cvolume;
                cvolume.scale (level.pa_volume);
                level.cvolume = cvolume;
                level.volume_pending = false;
                level.in_flight++;
                sent.push_tail (name);
                context.set_sink_input_volume (level.index, cvolume, on_set_done);
            }
            if (level.mute_pending) {
                level.mute_pending = false;
                level.in_flight++;
                sent.push_tail (name);
                context.set_sink_input_mute (level.index, level.muted, on_set_done);
            }
        }

        private void on_set_done (PulseAudio.Context c, int success) {
            var name = sent.pop_head ();
            var level = name != null ? levels[name] : null;
            // After the answer to the last change, fetch what the server made
            // of them all
            if (level != null && level.in_flight > 0 && --level.in_flight == 0 && level.live) {
                c.get_sink_input_info (level.index, on_sink_input_info);
            }
        }

        private void connect_server () {
            context = new PulseAudio.Context (loop.get_api (), Environment.get_application_name ());
            context.set_state_callback (on_state);
            context.set_subscribe_callback (on_event);
            if (context.connect (null, PulseAudio.Context.Flags.NOFAIL) < 0) {
                warning ("Can't connect to the sound server: %s", PulseAudio.strerror (context.errno ()));
            }
        }

        private void on_state (PulseAudio.Context c) {
            switch (c.get_state ()) {
            case PulseAudio.Context.State.READY:
                available = true;
                c.subscribe (PulseAudio.Context.SubscriptionMask.SINK_INPUT);
                c.get_sink_input_info_list (on_sink_input_info);
                break;
            case PulseAudio.Context.State.FAILED:
            case PulseAudio.Context.State.TERMINATED:
                // The context goes when we reconnect, not from its own callback
                available = false;
                forget_streams ();
                Timeout.add_seconds_once (RECONNECT_SECONDS, connect_server);
                break;
            default:
                break;
            }
        }

        private void on_event (PulseAudio.Context c, PulseAudio.Context.SubscriptionEventType t, uint32 idx) {
            var type = t & PulseAudio.Context.SubscriptionEventType.TYPE_MASK;
            if (type == PulseAudio.Context.SubscriptionEventType.REMOVE) {
                var name = indexes[idx];
                if (name != null) {
                    indexes.remove (idx);
                    var level = levels[name];
                    level.index = PulseAudio.INVALID_INDEX;
                    level.in_flight = 0;
                    changed (name);
                }
            } else {
                c.get_sink_input_info (idx, on_sink_input_info);
            }
        }

        private void on_sink_input_info (PulseAudio.Context c, PulseAudio.SinkInputInfo? info, int eol) {
            if (eol != 0 || info == null) {
                return;
            }
            var node = info.proplist.gets ("node.name");
            if (node == null || !node.has_prefix (NODE_PREFIX)) {
                return;
            }
            var name = node.substring (NODE_PREFIX.length);
            var level = get_level (name);
            var now = get_monotonic_time ();
            if (!level.live) {
                level.keep_until = now + RESTORE_USEC;
                level.keep_volume = level.volume_pending;
                level.keep_mute = level.mute_pending;
            }
            level.index = info.index;
            indexes[info.index] = name;
            if (level.in_flight == 0) {
                var keep = now < level.keep_until;
                level.cvolume = info.volume;
                if (level.volume_pending) {
                    // Sent below
                } else if (keep && level.keep_volume) {
                    level.volume_pending = info.volume.max () != level.pa_volume;
                } else {
                    level.volume = (double) info.volume.max () / PulseAudio.Volume.NORM;
                }
                var muted = info.mute != 0;
                if (level.mute_pending) {
                    // Sent below
                } else if (keep && level.keep_mute) {
                    level.mute_pending = muted != level.muted;
                } else {
                    level.muted = muted;
                }
            }
            apply (level);
            changed (name);
        }

        private void forget_streams () {
            indexes.remove_all ();
            sent.clear ();
            levels.foreach ((name, level) => {
                if (level.live) {
                    level.index = PulseAudio.INVALID_INDEX;
                    level.in_flight = 0;
                    changed (name);
                }
            });
        }

        /*
         * The volume WirePlumber saved for the VM, to show while its stream
         * is gone. Best effort: the file is WirePlumber's own. gerantd sets
         * application.id, which WirePlumber keys the volume by, and
         * WirePlumber stores linear volumes, which are cubed from the
         * percentages people see.
         */
        private static void load_saved (string vm_name, Level level) {
            var path = Path.build_filename (Environment.get_user_state_dir (), "wireplumber", "stream-properties");
            var key = "Output/Audio:application.id:org.garnish.vm." + vm_name;
            try {
                var file = new KeyFile ();
                file.load_from_file (path, KeyFileFlags.NONE);
                var value = file.get_value ("stream-properties", key.replace (" ", "\\s"));
                MatchInfo m;
                if (new Regex ("\"channelVolumes\": *\\[([^]]*)]").match (value, 0, out m)) {
                    double max = 0;
                    foreach (var v in m.fetch (1).split (",")) {
                        max = double.max (max, double.parse (v.strip ()));
                    }
                    level.volume = Math.cbrt (max).clamp (0, 1);
                }
                level.muted = value.contains ("\"mute\":true") || value.contains ("\"mute\": true");
            } catch (Error e) {
                // Never played, or not WirePlumber
            }
        }
    }
}
