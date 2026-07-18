# Original Engine Hashes

Страница фиксирует минимальный статический baseline, на который должны
ссылаться capture fixtures и Stage 4 evidence.

## Scope

- Источник: локальная статическая сверка Part 1 (`IS`) и Part 2 (`IS2`).
- Метод: SHA-256, export/import tables, `objdump -p`, `strings`.
- Эта страница не заменяет динамические traces: она задаёт only-if-match
  binary baseline для дальнейших runtime captures.

## Stable binaries across Part 1 / Part 2

| Binary | SHA-256 | Size | Notes |
| --- | --- | ---: | --- |
| `Ngi32.dll` | `bab9840d94f4e4e74ffc26677724fa896cf4823845504d09a9e025f80016edf5` | 253952 | Shared low-level render/resource/audio boundary |
| `World3D.dll` | `17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e` | 208896 | Shared gameplay/world/render lifecycle baseline |
| `Terrain.dll` | `6d3e68f0e15b297f6c184af3113baf1f31e19c3326c18f0150dec659242ed667` | 708608 | Shared terrain/shade/world baseline |
| `iron_3d.exe` / `iron_3d_p2.exe` | `f476af85c034a4b4f34f49d0806e4dff397b5da0ee26d382a7674231144979f7` | 36864 | Shared launcher binary |

## GOG research baseline

The canonical disassembly source is the windowed GOG installation, not the
Part 1/Part 2 test installations. Its `Terrain.dll` is a distinct revision:

| Binary | SHA-256 | Size | Notes |
| --- | --- | ---: | --- |
| `Terrain.dll` | `af87d1b2e728a0be73c52be3b44cc196ab46da7799f25a15d40f8c9b0b425ead` | 499712 | GOG camera receiver evidence; do not reuse Part 1/2 RVAs without a matching hash |

## Divergent binaries across Part 1 / Part 2

Эти модули нельзя автоматически считать behavior-compatible между частями:

- `AniMesh.dll`
- `Effect.dll`
- `iron3d.dll`
- `services.dll`
- `Control.dll`
- `ArealMap.dll`

## Practical use

1. Frame-order traces для `World3D.dll`, `Terrain.dll` и `Ngi32.dll` можно
   привязывать к shared profile, пока hash совпадает.
2. Animation and FX captures обязаны храниться раздельно для Part 1 и Part 2,
   потому что `AniMesh.dll` и `Effect.dll` отличаются.
3. Любой runtime fixture должен записывать минимум:
   - `game_part`
   - `module_name`
   - `module_sha256`
   - `mission`
   - `frame_or_tick`
   - `schema_version`

## Export / import focus for Stage 4

- `World3D.dll`: `stdCalculateGame`, `stdRenderGame`, `sendEndOfRender`
- `Terrain.dll`: `GetShade`, `GetWorld`, `stdSetCurrentCamera2`
- `AniMesh.dll`: `LoadAgent`, `LoadAniMesh`
- `Effect.dll`: `CreateFxManager`, `InitializeSettings`
- `Ngi32.dll`: `niGet3DRender`, `n3dPrimitive`, `n3dEndScene`, `rsLoadTexture`, `rsLoadMultiTexture`

Если будущий capture fixture не указывает, к какому hash он относится, такой
fixture нельзя считать acceptance evidence.
