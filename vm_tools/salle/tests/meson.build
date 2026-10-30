test_config = executable('test-config', 'test-config.vala',
  dependencies: core_dep,
  vala_args: ['--target-glib=2.80'],
)

# Each run starts from an empty configuration directory
test('config', find_program('sh'),
  args: ['-c', 'rm -rf "$XDG_CONFIG_HOME" && exec "$0"', test_config],
  env: [
    'XDG_CONFIG_HOME=' + meson.current_build_dir() / 'test-xdg-config',
    'HOME=/home/me',
  ],
)

test('apps', executable('test-apps', 'test-apps.vala',
  dependencies: core_dep,
  vala_args: ['--target-glib=2.80'],
))
