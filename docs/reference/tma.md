# TMA

`data.tma` -- основное описание расстановки и логической конфигурации миссии.
Файл перечисляет paths, clans, objects, свойства, ссылку на ландшафт и extras.

## String primitive

```c
struct LpString {
    uint32_t byte_length;
    uint8_t  bytes[byte_length];
};
```

Reader продвигается ровно на `4 + byte_length`. Завершающий NUL не является
обязательной частью framing. Для человекочитаемого вида используется legacy
ANSI/CP1251 view, но исходные bytes сохраняются.

## Top level

```text
u32 format_version
u32 path_count
PathRecord paths[path_count]
u32 clan_section_version
u32 clan_count
ClanRecord clans[clan_count]
u32 object_section_version
u32 object_count
PlacedObject objects[object_count]
LpString land_path
u32 mission_flag
LpString description_raw
u32 extra_section_version
u32 extra_count
ExtraRecord28 extras[extra_count]
```

Все 60 TMA Частей 1 и 2 проходят parser до точного EOF. Версии стабильны:
верхний уровень `1`, clan section `6`, object section `10`, property schema
`1`, trailing section `1`.

## PlacedObject

```text
u32      raw_kind
u32      class_or_flags
LpString resource_name
u32      raw_after_resource
u32      identity_or_clan_raw
f32      position[3]
f32      orientation[3]
f32      scale[3]
LpString instance_name
u32      raw_after_name
i32      link0
i32      link1
u32      property_schema_version
u32      property_count
Property properties[property_count]
```

Эти два слова после `resource_name` -- разные входные значения. В оригинале
`MisLoad.dll!FUN_10003900` (RVA `0x3900`) читает первое в смещение `0x54`
нативной записи, если версия секции объектов выше 2; следующее слово попадает
в смещение `0x5c` только при версии выше 3. Getter
`MisLoad.dll!FUN_10001440` (RVA `0x1440`) копирует первое слово в смещение
`0x50` строки, передаваемой хосту. `iron3d.dll!FUN_100a1f30` (RVA `0xA1F30`)
загружает миссию и вызывает `FUN_100a3e40` (RVA `0xA3E40`); тот запрашивает
строку через vtable slot `+0x28` (callsite RVA `0xA3F1A`). Хост передаёт это
значение в создание схемы объекта в ArealMap, откуда оно становится первым
DWORD дескриптора создания World3D. `World3D.dll!CreateObject` (RVA `0x7CB0`)
читает DWORD по RVA `0x7E58`, а callsite RVA `0x7E62` передаёт его через vtable
slot `+0x40` в путь выбора AI в Behavior. Поэтому Rust сохраняет первое слово
как `raw_after_resource: u32`: оригинал использует его как индекс выбора AI,
но соответствие значения типу `OwnerId` в `fparkan-world` не доказано.
Соседнее `identity_or_clan_raw` остаётся отдельным raw-полем; его имя не
доказывает, кому объект принадлежит.

Цепочка проверена на этих сборках (SHA-256): `MisLoad.dll`
`c607762bb67db14ca530927c8e1347339b62b0a163ac90c708ee83142292e2bf`,
`iron3d.dll`
`d1dc4ea8535e2069b0a05c9e8be2724dd438ba44cca7a83007d8b2e50f9e35fa`,
`ArealMap.dll`
`a25f9254ebb339d70e2e07c8121950191e9584ca0858d8e9c2e2b6c671edf6ea`,
`World3D.dll`
`17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e` и
`Behavior.dll`
`ddfbdbde07832358fe6e35e01c8dba9b58e7313789498b6049ed51d4ed9baf99`.
Таблица `GetSuperAI` в `ai.dll` находится по RVA `0x55398`; SHA-256 файла —
`c19cd6cd31cd2896f8c4ba0f0e700dd3408aec8182daaf4863d98a7d233a2abc`.

`Property` состоит из четырёх raw `u32` и имени. Typed views разрешены только
для доказанных property names и consumers.

Для свойств объекта `MisLoad.dll!FUN_10003ab0` (RVA `0x3ab0`) переставляет
дисковые слова в runtime-запись, а `FUN_100040b0` (RVA `0x40b0`) проходит её
таблицу в порядке runtime-дескрипторов. Поиск имени учитывает длину и байты с
точным регистром и использует первое совпадение. В подтверждённом пути
дисковое `raw_value[1]` становится первым DWORD аргумента setter-а, поэтому
значение передаётся как биты, без промежуточного float-преобразования.
Например, Behavior регистрирует `CurrentOre` под кодом `5`; его setter кладёт
полученные биты в основное слово свойства `0x02000100`. `MaximumOre` с кодом
`4` читает дополнительное слово той же записи, а setter этого дескриптора
ничего не меняет. Подтверждение классов и native getter/setter находится в
[главе о Behavior](../tomes/06-behavior.md#начальные-свойства-запасов-руды).
