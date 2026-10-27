project('salle', ['c', 'vala'],
          version: '0.1.0',
    meson_version: '>= 1.0.0',
  default_options: ['warning_level=2', 'werror=false'],
)

i18n = import('i18n')
gnome = import('gnome')

base_id = 'org.garnish.Salle'

if get_option('profile') == 'development'
  application_id = '@0@.Devel'.format(base_id)
  vcs_tag = run_command('git', 'rev-parse', '--short', 'HEAD', check: false).stdout().strip()
  version_suffix = vcs_tag == '' ? '-devel' : '-@0@'.format(vcs_tag)
else
  application_id = base_id
  version_suffix = ''
endif

resource_path = '/org/garnish/Salle'

conf = configuration_data()
conf.set_quoted('APPLICATION_ID', application_id)
conf.set_quoted('VERSION', meson.project_version() + version_suffix)
conf.set_quoted('PROFILE', get_option('profile'))
conf.set_quoted('RESOURCE_PATH', resource_path)
conf.set_quoted('GETTEXT_PACKAGE', meson.project_name())
conf.set_quoted('LOCALEDIR', get_option('prefix') / get_option('localedir'))
configure_file(output: 'config.h', configuration: conf)

add_project_arguments(
  '-DGETTEXT_PACKAGE="@0@"'.format(meson.project_name()),
  '-w',
  language: 'c',
)
config_inc = include_directories('.')

subdir('data')
subdir('src')
subdir('po')
subdir('tests')

gnome.post_install(
  glib_compile_schemas: true,
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)
