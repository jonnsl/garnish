desktop_conf = configuration_data()
desktop_conf.set('APP_ID', application_id)

desktop_file = i18n.merge_file(
  input: configure_file(
    input: 'org.garnish.Salle.desktop.in.in',
    output: '@BASENAME@',
    configuration: desktop_conf,
  ),
  output: '@0@.desktop'.format(application_id),
  type: 'desktop',
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'applications',
)

desktop_utils = find_program('desktop-file-validate', required: false)
if desktop_utils.found()
  test('validate-desktop', desktop_utils, args: [desktop_file])
endif

metainfo_file = i18n.merge_file(
  input: configure_file(
    input: 'org.garnish.Salle.metainfo.xml.in.in',
    output: '@BASENAME@',
    configuration: desktop_conf,
  ),
  output: '@0@.metainfo.xml'.format(application_id),
  po_dir: '../po',
  install: true,
  install_dir: get_option('datadir') / 'metainfo',
)

appstreamcli = find_program('appstreamcli', required: false)
if appstreamcli.found()
  test('validate-metainfo', appstreamcli,
    args: ['validate', '--no-net', '--explain', metainfo_file],
  )
endif

gschema = configure_file(
  input: 'org.garnish.Salle.gschema.xml.in',
  output: '@0@.gschema.xml'.format(application_id),
  configuration: desktop_conf,
  install: true,
  install_dir: get_option('datadir') / 'glib-2.0' / 'schemas',
)

compile_schemas = find_program('glib-compile-schemas')
test('validate-schema', compile_schemas,
  args: ['--strict', '--dry-run', meson.current_build_dir()],
)

# Compiled into the build dir so the app can run uninstalled with GSETTINGS_SCHEMA_DIR=build/data
custom_target('gschemas.compiled',
  input: gschema,
  output: 'gschemas.compiled',
  command: [compile_schemas, '--strict', '--targetdir=@OUTDIR@', '@OUTDIR@'],
  build_by_default: true,
)

subdir('icons')
