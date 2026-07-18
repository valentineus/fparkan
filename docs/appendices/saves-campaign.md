# Кампания, сохранения и восстановление сессии

## Известная файловая поверхность

Demo содержит `MISSIONS/dispatcher.ini` и `SAVE/saveslots.cfg`.
`dispatcher.ini` хранит campaign progression (в demo — `[COMPLETE]`), а
`saveslots.cfg` — ordered UI-метаданные slots, не полный snapshot мира.
В Части 2 slots 1 и 7 помечены занятыми без соответствующего payload; поэтому
проверяются независимо: наличие metadata record, физического файла и
format/version/integrity payload.

Campaign различает существование миссии, её доступность, старт, успешное или
неуспешное завершение и уже применённый результат. Обработка одного
mission-complete event идемпотентна. Пустой slot и повреждённый существующий
payload — разные состояния.

## Контракт standalone save

Полный snapshot сохраняет campaign/mission context, time/pause/step phase,
stable object IDs, ownership, transforms, lifecycle/properties/cross-links,
world changes, Behavior/Control/AI/research/economy state, script variables/IP/
timers, authoritative RNG, gameplay-relevant FX и queued messages. Camera/UI
можно сохранить как presentation context; GPU/audio handles и draw buffers
восстанавливаются, а не сериализуются.

Снимок разрешён только после calculation и deferred operations, вне queue
traversal, после применённых network messages и до чтения mutable state
renderer-ом. Native pointers, vtable/allocator addresses и resource mapping
pointers запрещены: ссылки идут через stable IDs и resource keys.

Новый формат — versioned chunks (`WORLD`, `OBJECTS`, `BEHAVIOR`, `PHYSICS`,
`AI`, `SCRIPT`, `RESEARCH`, `RNG`, `CAMERA_UI`, optional network) с magic,
format/profile/content fingerprint, size и checksum. Неизвестный optional
chunk пропускается; required chunk блокирует load. Это дизайн FParkan, не
утверждение о binary format оригинала.

Запись транзакционна: временный файл → повторное чтение/checksum → fs sync →
атомарная замена payload → обновление slot metadata. Загрузка создаёт mission
и objects без публикации, восстанавливает IDs/cross-links/controllers/RNG,
регистрирует их, валидирует и только затем разрешает следующий tick.

## Проверки и граница

`save -> load` обязан давать тот же canonical state hash, OriginalObjectId,
cross-links и продолжение RNG. Corrupt required chunk не публикует частичный
мир; crash при записи не уничтожает старый slot; completion event идемпотентен.

Для native format нужны controlled original saves, binary diffs и trace
serializer-а. До этого FParkan может иметь совместимую семантику собственных
saves, но не заявляет byte/network interoperability с оригинальными файлами.
