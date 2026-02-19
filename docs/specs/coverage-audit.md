# Documentation coverage audit

Дата аудита: `2026-02-19`  
Корпус данных: `testdata/Parkan - Iron Strategy`

## 1. Проверка форматов архивов

Результаты:

- `NRes`: `120` архивов, roundtrip `120/120` (byte-identical)
- `RsLi`: `2` архива, roundtrip `2/2` (byte-identical)
- подтвержден один совместимый quirk: `sprites.lib`, entry `23`, `deflate EOF+1`

Инструмент:

- `tools/archive_roundtrip_validator.py`

## 2. Проверка рендерных форматов

Результаты:

- `MSH`: `435/435` валидны
- `Texm`: `518/518` валидны
- `FXID`: `923/923` валидны
- `Terrain/Map` (`Land.msh` + `Land.map`): `33/33` без ошибок/предупреждений

Инструменты:

- `tools/msh_doc_validator.py`
- `tools/fxid_abs100_audit.py`
- `tools/terrain_map_doc_validator.py`

## 3. Глобальный статус по подсистемам

| Подсистема | Статус | Что блокирует 100% |
|---|---|---|
| Архивы (`NRes`, `RsLi`) | практически закрыта | формализация редких не-ASCII/служебных edge-case |
| 3D geometry (`MSH core`) | высокая готовность | семантика opaque-полей и канонический writer «с нуля» |
| Animation (`Res8/Res19`) | высокая готовность | полный FP-parity на всех edge-case |
| Material/Wear/Texture | высокая готовность | полная field-level семантика служебных флагов и writer-профиль |
| FXID | высокая готовность | полная field-level семантика payload по каждому opcode |
| Terrain/Areal map formats | высокая готовность | доменная семантика `class_id/logic_flag`, ветка `poly_count>0` |
| Render pipeline | хорошая | полный pixel-parity набор эталонных кадров в CI |
| AI/Behavior/Control/Missions/UI/Sound/Network | начальное покрытие | требуется полная спецификация форматов и runtime-контрактов |

## 4. План доведения до 100%

1. Закрыть field-level семантику opaque/служебных полей в 3D/FX/terrain подсистемах.
2. Завершить canonical writer paths для авторинга новых ассетов без copy-through.
3. Зафиксировать и автоматизировать pixel/frame parity-критерии в CI.
4. Расширить подсистемные спецификации (`AI`, `Behavior`, `Missions`, `Control`, `UI`, `Sound`, `Network`) до уровня «полный формат + полный runtime-контракт + parity-тесты».
