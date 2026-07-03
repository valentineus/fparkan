# Stage 4 Capture Schema

Stage 4 нельзя закрывать набором ad-hoc логов. Нужна схема, по которой
animation, FX и rendered frame captures сравниваются между Part 1, Part 2 и
современной реализацией.

## Goals

- сделать captures пригодными для автоматического diff;
- не хранить host-specific пути, временные каталоги и нестабильные handles;
- связывать frame traces, command captures и pixel artifacts общим identity.

## Common envelope

```json
{
  "schema_version": "fparkan-stage4-capture-v1",
  "capture_kind": "frame-trace | animation-pose | fx-lifecycle | render-frame",
  "game_part": "part1 | part2",
  "mission": "MISSIONS/.../data.tma",
  "frame_id": 123,
  "tick": 123,
  "module_hashes": {
    "World3D.dll": "sha256...",
    "Terrain.dll": "sha256...",
    "AniMesh.dll": "sha256...",
    "Effect.dll": "sha256..."
  },
  "tool_version": "codex/manual/fixture version",
  "notes": []
}
```

## Capture kinds

### `frame-trace`

Используется для порядка фаз и внешних вызовов.

Required fields:

- `events`: ordered list of `{ phase, symbol, sequence, object_id?, fx_id?, camera_id? }`
- `queue_counters`: deferred operations, visible objects, emitted FX, UI callbacks
- `rng_state`: optional, if recoverable

### `animation-pose`

Используется для x87 / portable sampler parity.

Required fields:

- `clip_id`
- `node_index`
- `sample_time`
- `numeric_profile`
- `translation`
- `rotation_quat`
- `scale`
- `matrix_hash`

### `fx-lifecycle`

Используется для create/update/emit/stop parity.

Required fields:

- `fx_id`
- `instance_id`
- `time`
- `opcode_events`
- `rng_calls`
- `resource_refs`
- `emissions`

### `render-frame`

Связывает backend-neutral snapshot с live Vulkan output.

Required fields:

- `camera`
- `visible_object_ids`
- `draws`
- `pipeline_keys`
- `resource_ids`
- `validation`
- `pixel_artifact`

## Stability rules

1. Не записывать абсолютные host paths.
2. Не записывать raw pointer addresses как identity fields.
3. `frame_id` должен совпадать между trace, command capture и pixel artifact.
4. GPU-specific transient handles допустимы только внутри diagnostics fields и
   не участвуют в canonical equality.
5. Любой capture without `module_hashes` считается informational, а не
   acceptance-grade.

## Acceptance mapping

- `S4-TRACE-*` rows читают `frame-trace`
- `S4-ANIM-*` rows читают `animation-pose`
- `S4-FX-*` rows читают `fx-lifecycle`
- `S4-VK-*` и `S4-PIXEL-*` rows читают `render-frame`

Эта схема intentionally минимальна. Новые поля можно добавлять, но нельзя
ломать перечисленные identity and parity anchors без смены `schema_version`.
