# 3D implementation notes

Контрольные заметки, сводки алгоритмов и остаточные семантические вопросы по 3D-подсистемам.

---

## 5.1. Порядок байт

Все значения хранятся в **little‑endian** порядке (платформа x86/Win32).

## 5.2. Выравнивание

- **NRes‑ресурсы:** данные каждого ресурса внутри NRes‑архива выровнены по границе **8 байт** (0‑padding).
- **Внутренняя структура ресурсов:** таблицы Res1/Res2/Res7/Res13 не имеют межзаписевого выравнивания — записи идут подряд.
- **Vertex streams:** stride'ы фиксированы (12/4/8 байт) — вершинные данные идут подряд без паддинга.

## 5.3. Размеры записей на диске

| Ресурс | Запись    | Размер (байт) | Stride                  |
|--------|-----------|---------------|-------------------------|
| Res1   | Node      | 38            | 38 (19×u16)             |
| Res2   | Slot      | 68            | 68                      |
| Res3   | Position  | 12            | 12 (3×f32)              |
| Res4   | Normal    | 4             | 4 (4×s8)                |
| Res5   | UV0       | 4             | 4 (2×s16)               |
| Res6   | Index     | 2             | 2 (u16)                 |
| Res7   | TriDesc   | 16            | 16                      |
| Res8   | AnimKey   | 24            | 24                      |
| Res10  | StringRec | переменный    | `4 + (len ? len+1 : 0)` |
| Res13  | Batch     | 20            | 20                      |
| Res19  | AnimMap   | 2             | 2 (u16)                 |
| Res15  | VtxStr    | 8             | 8                       |
| Res16  | VtxStr    | 8             | 8 (2×4)                 |
| Res18  | VtxStr    | 4             | 4                       |

## 5.4. Вычисление количества элементов

Количество записей вычисляется из размера ресурса:

```
count = resource_data_size / record_stride
```

Например:

- `vertex_count = res3_size / 12`
- `index_count  = res6_size / 2`
- `batch_count  = res13_size / 20`
- `slot_count   = (res2_size - 140) / 68`
- `node_count   = res1_size / 38`
- `tri_desc_count = res7_size / 16`
- `anim_key_count = res8_size / 24`
- `anim_map_count = res19_size / 2`

Для Res10 нет фиксированного stride: нужно последовательно проходить записи `u32 len` + `(len ? len+1 : 0)` байт.

## 5.5. Идентификация ресурсов в NRes

Ресурсы модели идентифицируются по полю `type` (смещение 0) в каталожной записи NRes. Загрузчик использует `niFindRes(archive, type, subtype)` для поиска, где `type` — число (1, 2, 3, ... 20), а `subtype` (byte) — уточнение (из аргумента загрузчика).

## 5.6. Минимальный набор для рендера

Для статической модели без анимации достаточно:

| Ресурс | Обязательность                                 |
|--------|------------------------------------------------|
| Res1   | Да                                             |
| Res2   | Да                                             |
| Res3   | Да                                             |
| Res4   | Рекомендуется                                  |
| Res5   | Рекомендуется                                  |
| Res6   | Да                                             |
| Res7   | Для коллизии                                   |
| Res13  | Да                                             |
| Res10  | Желательно (узловые имена/поведенческие ветки) |
| Res8   | Нет (анимация)                                 |
| Res19  | Нет (анимация)                                 |
| Res15  | Нет                                            |
| Res16  | Нет                                            |
| Res18  | Нет                                            |
| Res20  | Нет                                            |

## 5.7. Сводка алгоритмов декодирования

### Позиции (Res3)

```python
def decode_position(data, vertex_index):
    offset = vertex_index * 12
    x = struct.unpack_from('<f', data, offset)[0]
    y = struct.unpack_from('<f', data, offset + 4)[0]
    z = struct.unpack_from('<f', data, offset + 8)[0]
    return (x, y, z)
```

### Нормали (Res4)

```python
def decode_normal(data, vertex_index):
    offset = vertex_index * 4
    nx = struct.unpack_from('<b', data, offset)[0]      # int8
    ny = struct.unpack_from('<b', data, offset + 1)[0]
    nz = struct.unpack_from('<b', data, offset + 2)[0]
    # nw = data[offset + 3]  # не используется
    return (
        max(-1.0, min(1.0, nx / 127.0)),
        max(-1.0, min(1.0, ny / 127.0)),
        max(-1.0, min(1.0, nz / 127.0)),
    )
```

### UV‑координаты (Res5)

```python
def decode_uv(data, vertex_index):
    offset = vertex_index * 4
    u = struct.unpack_from('<h', data, offset)[0]        # int16
    v = struct.unpack_from('<h', data, offset + 2)[0]
    return (u / 1024.0, v / 1024.0)
```

### Кодирование нормали (для экспортёра)

```python
def encode_normal(nx, ny, nz):
    return (
        max(-128, min(127, int(round(nx * 127.0)))),
        max(-128, min(127, int(round(ny * 127.0)))),
        max(-128, min(127, int(round(nz * 127.0)))),
        0   # nw = 0 (безопасное значение)
    )
```

### Кодирование UV (для экспортёра)

```python
def encode_uv(u, v):
    return (
        max(-32768, min(32767, int(round(u * 1024.0)))),
        max(-32768, min(32767, int(round(v * 1024.0))))
    )
```

### Строки узлов (Res10)

```python
def parse_res10_for_nodes(buf: bytes, node_count: int) -> list[str | None]:
    out = []
    off = 0
    for _ in range(node_count):
        ln = struct.unpack_from('<I', buf, off)[0]
        off += 4
        if ln == 0:
            out.append(None)
            continue
        raw = buf[off:off + ln + 1]     # len + '\0'
        out.append(raw[:-1].decode('ascii', errors='replace'))
        off += ln + 1
    return out
```

### Ключ анимации (Res8) и mapping (Res19)

```python
def decode_anim_key24(buf: bytes, idx: int):
    o = idx * 24
    px, py, pz, t = struct.unpack_from('<4f', buf, o)
    qx, qy, qz, qw = struct.unpack_from('<4h', buf, o + 16)
    s = 1.0 / 32767.0
    return (px, py, pz), t, (qx * s, qy * s, qz * s, qw * s)
```

### Эффектный поток (FXID)

```python
FX_CMD_SIZE = {1:224,2:148,3:200,4:204,5:112,6:4,7:208,8:248,9:208,10:208}

def parse_fx_payload(raw: bytes):
    cmd_count = struct.unpack_from('<I', raw, 0)[0]
    ptr = 0x3C
    cmds = []
    for _ in range(cmd_count):
        w = struct.unpack_from('<I', raw, ptr)[0]
        op = w & 0xFF
        enabled = (w >> 8) & 1
        size = FX_CMD_SIZE[op]
        cmds.append((op, enabled, ptr, size))
        ptr += size
    if ptr != len(raw):
        raise ValueError('tail bytes after command stream')
    return cmds
```

### Texm (header + mips + Page)

```python
def parse_texm(raw: bytes):
    magic, w, h, mips, f4, f5, unk6, fmt = struct.unpack_from('<8I', raw, 0)
    assert magic == 0x6D786554  # 'Texm'
    bpp = 1 if fmt == 0 else (2 if fmt in (565, 556, 4444) else 4)
    pix_sum = 0
    mw, mh = w, h
    for _ in range(mips):
        pix_sum += mw * mh
        mw = max(1, mw >> 1)
        mh = max(1, mh >> 1)
    off = 32 + (1024 if fmt == 0 else 0) + bpp * pix_sum
    page = None
    if off + 8 <= len(raw) and raw[off:off+4] == b'Page':
        n = struct.unpack_from('<I', raw, off + 4)[0]
        page = [struct.unpack_from('<4h', raw, off + 8 + i * 8) for i in range(n)]
    return (w, h, mips, fmt, f4, f5, unk6, page)
```

---

# Часть 6. Остаточные семантические вопросы

Пункты ниже **не блокируют 1:1-парсинг/рендер/интерполяцию** (все бинарные структуры уже определены), но их человеко‑читаемая трактовка может быть уточнена дополнительно.

## 6.1. Batch table — смысл `unk4/unk6/unk14`

Физическое расположение полей известно, но доменное имя/назначение не зафиксировано:

- `unk4` (`+0x04`)
- `unk6` (`+0x06`)
- `unk14` (`+0x0E`)

## 6.2. Node flags и имена групп

- Биты в `Res1.hdr0` используются в ряде рантайм‑веток, но их «геймдизайн‑имена» неизвестны.
- Для group‑индекса `0..4` не найдено текстовых label'ов в ресурсах; для совместимости нужно сохранять числовой индекс как есть.

## 6.3. Slot tail `unk30..unk40`

Хвост слота (`+0x30..+0x43`, `5×uint32`) стабильно присутствует в формате, но движок не делает явной семантической декомпозиции этих пяти слов в path'ах загрузки/рендера/коллизии.

## 6.4. Effect command payload semantics

Container/stream формально полностью восстановлен (header, opcode, размеры, инстанцирование). Остаётся необязательная задача: дать «человеко‑читаемые» имена каждому полю внутри payload конкретных opcode.

## 6.5. Поля `TexmHeader.flags4/flags5/unk6`

Бинарный layout и декодер известны, но значения этих трёх полей в контенте используются контекстно; для 1:1 достаточно хранить/восстанавливать их без модификации.

## 6.6. Что пока не хватает для полноценного обратного экспорта (`OBJ -> MSH/NRes`)

Ниже перечислено то, что нужно закрыть для **lossless round-trip** и 1:1‑поведения при импорте внешней геометрии обратно в формат игры.

### A) Неполная «авторская» семантика бинарных таблиц

1. `Res2` header (`первые 0x8C`): не зафиксированы все поля и правила их вычисления при генерации нового файла (а не copy-through из оригинала).
2. `Res7` tri-descriptor: для 16‑байтной записи декодирован базовый каркас, но остаётся неформализованной часть служебных бит/полей, нужных для стабильной генерации adjacency/служебной топологии.
3. `Res13` поля `unk4/unk6/unk14`: для парсинга достаточно, но для генерации «канонических» значений из голого `OBJ` правила не определены.
4. `Res2` slot tail (`unk30..unk40`): семантика не разложена, поэтому при экспорте новых ассетов нет детерминированной формулы заполнения.

### B) Анимационный path ещё не закрыт как writer

1. Нужен полный writer для `Res8/Res19`:
   - точная спецификация байтового формата на запись;
   - правила генерации mapping (`Res19`) по узлам/кадрам;
   - жёсткая фиксация округления как в x87 path (включая edge-case на границах кадра).
2. Правила биндинга узлов/строк (`Res10`) и `slotFlags` к runtime‑сущностям пока описаны частично и требуют формализации именно для импорта новых данных.

### C) Материалы, текстуры, эффекты для «полного ассета»

1. Для `Texm` не завершён writer, покрывающий все используемые режимы (включая palette path, mip-chain, `Page`, и правила заполнения служебных полей).
2. Для `FXID` известен контейнер/длины команд, но не завершена field-level семантика payload всех opcode для генерации новых эффектов, эквивалентных оригинальному пайплайну.
3. Экспорт только `OBJ` покрывает геометрию; для игрового ассета нужен sidecar-слой (материалы/текстуры/эффекты/анимация), иначе импорт неизбежно неполный.

### D) Что это означает на практике

1. `OBJ -> MSH` сейчас реалистичен как **ограниченный static-экспорт** (позиции/индексы/часть batch/slot структуры).
2. `OBJ -> полноценный игровой ресурс` (без потерь, с поведением 1:1) пока недостижим без закрытия пунктов A/B/C.
3. До закрытия пунктов A/B/C рекомендуется использовать режим:
   - геометрия экспортируется из `OBJ`;
   - неизвестные/служебные поля берутся copy-through из референсного оригинального ассета той же структуры.
