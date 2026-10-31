blueprints = custom_target('blueprints',
  input: files(
    'dialogs/log-dialog.blp',
    'dialogs/vm-dialog.blp',
    'pages/apps-page.blp',
    'pages/vm-page.blp',
    'shortcuts-dialog.blp',
    'widgets/vm-row.blp',
    'window.blp',
  ),
  output: '.',
  command: [
    find_program('blueprint-compiler'),
    'batch-compile', '@OUTPUT@', '@CURRENT_SOURCE_DIR@', '@INPUT@',
  ],
)

resources = gnome.compile_resources('salle-resources',
  'salle.gresource.xml',
  c_name: 'salle',
  dependencies: blueprints,
)

deps = [
  dependency('gtk4', version: '>= 4.18'),
  dependency('libadwaita-1', version: '>= 1.8'),
  dependency('gio-2.0', version: '>= 2.80'),
  dependency('json-glib-1.0'),
  meson.get_compiler('vala').find_library('config', dirs: meson.current_source_dir()),
]

# Models and services, without UI, shared with the tests
core_lib = static_library('salle-core',
  files(
    'models/app-catalog.vala',
    'models/vm-config.vala',
    'models/vm.vala',
    'services/ini-file.vala',
    'services/gerant.vala',
    'utils.vala',
  ),
  dependencies: deps,
  include_directories: config_inc,
  vala_args: ['--target-glib=2.80'],
)
core_dep = declare_dependency(
  link_with: core_lib,
  dependencies: deps,
  include_directories: [config_inc, include_directories('.')],
)

sources = files(
  'application.vala',
  'dialogs/log-dialog.vala',
  'dialogs/vm-dialog.vala',
  'main.vala',
  'pages/apps-page.vala',
  'pages/vm-page.vala',
  'services/mixer.vala',
  'vm-actions.vala',
  'widgets/vm-row.vala',
  'window.vala',
)

mixer_deps = [
  dependency('libpulse'),
  dependency('libpulse-mainloop-glib'),
  meson.get_compiler('c').find_library('m'),
]

executable('salle', sources, resources,
  dependencies: [deps, core_dep, mixer_deps],
  include_directories: config_inc,
  vala_args: [
    '--target-glib=2.80',
    '--gresourcesdir=' + meson.current_build_dir(),
  ],
  install: true,
)
