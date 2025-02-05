nres_src = files('src/nres.c')
nres_inc = include_directories('include')

nres_lib = library('nres', nres_src,
  include_directories: nres_inc,
  install: true
)

nres_dep = declare_dependency(
  include_directories: nres_inc,
  link_with: nres_lib
)

meson.override_dependency('nres_dep', nres_dep)