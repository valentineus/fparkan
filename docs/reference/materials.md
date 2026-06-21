# WEAR и MAT0

MSH batch хранит только `material_index`. WEAR переводит этот индекс в имя
материала, а MAT0 по этому имени описывает phases, parameters и texture
references.

```text
Batch20.material_index
  -> WEAR row
  -> MAT0 entry
  -> active phase
  -> textureName
```

## WEAR

WEAR -- текстовый ресурс type ID `0x52414557`, обычно `*.wea` рядом с моделью.

```text
<wearCount>
<legacyId> <materialName>
...

[empty line]
[LIGHTMAPS
<lightmapCount>
<legacyId> <lightmapName>
...]
```

`legacyId` сохраняется, но выбор выполняется по позиции строки и имени. Между
основной таблицей и `LIGHTMAPS` нужен пустой разделитель.

## MAT0

MAT0 имеет type ID `0x3054414D`, обычно расположен в `Material.lib`. `attr1`
содержит runtime flags, `attr2` -- версию payload.

```c
#pragma pack(push, 1)
struct Mat0PrefixV4Plus {
    uint16_t phase_count;
    uint16_t animation_block_count;
    uint8_t  metadata_a;
    uint8_t  metadata_b;
    uint32_t metadata_c_raw;
    uint32_t metadata_d_raw;
};

struct Phase34 {
    uint8_t parameters[18];
    char texture_name[16];
};
#pragma pack(pop)
```

Versioned fields читаются только если версия их содержит. Для старых версий
используются runtime defaults, а raw values сохраняются.

## Fallback

Material resolve:

1. имя из WEAR;
2. `DEFAULT`;
3. entry с индексом 0.

Пустое texture name означает намеренно нетекстурированную поверхность. Lightmap
fallback отдельный: отсутствующий lightmap даёт slot `-1`.
