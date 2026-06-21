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

`Property` состоит из четырёх raw `u32` и имени. Typed views разрешены только
для доказанных property names и consumers.
