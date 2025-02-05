nres_dep = dependency('nres_dep', fallback: ['nres', 'nres_dep'])

executable('nres_tool',
  sources : files('src/main.c'),
  dependencies: [nres_dep],
  install: true
)