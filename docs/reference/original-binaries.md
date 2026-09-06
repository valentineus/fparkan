# Оригинальные модули

RVA в книге относится к определённой сборке DLL. SHA-256 позволяет
проверить, что сравнивается тот же код.

## Совпадающие модули частей 1 и 2

| Binary | SHA-256 | Size | Notes |
| --- | --- | ---: | --- |
| `Ngi32.dll` | `bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5` | 253952 | Shared low-level render/resource/audio boundary |
| `World3D.dll` | `17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e` | 208896 | Shared gameplay/world/render lifecycle baseline |
| `Terrain.dll` | `6d3e68f0e15b297f6c184af3113baf1f31e19c3326c18f0150dec659242ed667` | 708608 | Shared terrain/shade/world baseline |
| `iron_3d.exe` / `iron_3d_p2.exe` | `f476af85c034a4b4f34f49d0806e4dff397b5da0ee26d382a7674231144979f7` | 36864 | Shared launcher binary |

## Сборка GOG

The canonical disassembly source is the windowed GOG installation, not the
Part 1/Part 2 test installations. Its `Terrain.dll` is a distinct revision:

| Binary | SHA-256 | Size | Notes |
| --- | --- | ---: | --- |
| `Terrain.dll` | `af87d1b2e728a0be73c52be3b44cc196ab46da7799f25a15d40f8c9b0b425ead` | 499712 | GOG camera receiver evidence; do not reuse Part 1/2 RVAs without a matching hash |

## Различающиеся модули

Эти модули нельзя автоматически считать behavior-compatible между частями:

- `AniMesh.dll`
- `Effect.dll`
- `iron3d.dll`
- `services.dll`
- `Control.dll`
- `ArealMap.dll`
