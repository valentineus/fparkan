# FXID

Документ фиксирует спецификацию ресурса эффекта `FXID` на уровне, достаточном для:

- 1:1 загрузки и исполнения в совместимом runtime;
- построения валидатора payload;
- создания lossless-конвертера (`binary -> IR -> binary`);
- создания редактора с безопасным редактированием полей.

Связанный контейнер: [NRes / RsLi](nres.md).

---

## 1. Источники и статус восстановления

Спецификация восстановлена по:

- `tmp/disassembler1/Effect.dll.c`;
- `tmp/disassembler2/Effect.dll.asm`;
- интеграционным вызовам из `tmp/disassembler1/Terrain.dll.c`;
- проверке реальных архивов `testdata/nres`.

Ключевые функции:

- parser FXID: `Effect.dll!sub_10007650`;
- runtime loop: `sub_10003D30(case 28)`, `sub_10006170`, `sub_10008120`, `sub_10007D10`;
- alpha/time: `sub_10005C60`;
- exports: `CreateFxManager`, `InitializeSettings`.

Проверка по данным:

- `923/923` FXID payload валидны в `testdata/nres`.

---

## 2. Контейнер и runtime API

### 2.1. NRes entry

FXID хранится как NRes-entry:

- `type_id = 0x44495846` (`"FXID"`).

Наблюдение по датасету (923 эффекта):

- `attr1 = 0`, `attr2 = 0`, `attr3 = 1`.

### 2.2. Export API `Effect.dll`

Экспортируются:

- `CreateFxManager(int a1, int a2, int owner)`;
- `InitializeSettings()`.

`CreateFxManager` создаёт manager-объект (`0xB8` байт), инициализирует через `sub_10003AE0`, возвращает интерфейсный указатель (`base + 4`).

### 2.3. Интерфейс менеджера

Рабочая vtable (`off_1001E478`):

| Смещение | Функция | Назначение |
|---|---|---|
| +0x08 | `sub_10003D30` | Event dispatcher (`4/20/23/24/28`) |
| +0x10 | `sub_10004320` | Открыть/закэшировать FX resource |
| +0x14 | `sub_10004590` | Создать runtime instance |
| +0x18 | `sub_10004780` | Удалить instance |
| +0x1C | `sub_100047B0` | Установить time/interp mode |
| +0x20 | `sub_100047D0` | Установить scale |
| +0x24 | `sub_10004830` | Установить позицию |
| +0x28 | `sub_10004930` | Установить matrix transform |
| +0x2C | `sub_10004B00` | Restart/retime |
| +0x38 | `sub_10004BA0` | Duration modifier |
| +0x3C | `sub_10004BD0` | Start/Enable |
| +0x40 | `sub_10004C10` | Stop/Disable |
| +0x44 | `sub_10004C50` | Bind emitter/context |
| +0x48 | `sub_10004D50` | Сброс frame flags |

`Terrain.dll` использует `QueryInterface(id=19)` для получения рабочего интерфейса.

---

## 3. Бинарный формат FXID payload

Все значения little-endian.

### 3.1. Header (60 байт, `0x3C`)

```c
struct FxHeader60 {
    uint32_t cmd_count;      // 0x00
    uint32_t time_mode;      // 0x04
    float    duration_sec;   // 0x08
    float    phase_jitter;   // 0x0C
    uint32_t flags;          // 0x10
    uint32_t settings_id;    // 0x14
    float    rand_shift_x;   // 0x18
    float    rand_shift_y;   // 0x1C
    float    rand_shift_z;   // 0x20
    float    pivot_x;        // 0x24
    float    pivot_y;        // 0x28
    float    pivot_z;        // 0x2C
    float    scale_x;        // 0x30
    float    scale_y;        // 0x34
    float    scale_z;        // 0x38
};
```

Командный поток начинается строго с `offset = 0x3C`.

### 3.2. Header-поля (подтвержденная семантика)

- `cmd_count`: число команд (engine итерирует ровно столько шагов).
- `time_mode`: базовый режим вычисления alpha/time (`sub_10005C60`).
- `duration_sec`: в runtime -> `duration_ms = duration_sec * 1000`.
- `phase_jitter`: используется при `flags & 0x1`.
- `flags`: runtime-gating/alpha/visibility (см. ниже).
- `settings_id`: в `sub_1000EC40` используется `settings_id & 0xFF`.
- `rand_shift_*`: используется при `flags & 0x8`.
- `pivot_*`: используется в ветках `sub_10007D10`.
- `scale_*`: копируется в runtime scale и влияет на матрицы.

### 3.3. `flags` (битовая карта)

| Бит | Маска | Наблюдаемое поведение |
|---|---:|---|
| 0 | `0x0001` | Random phase jitter (`phase_jitter`) |
| 3 | `0x0008` | Random positional shift (`rand_shift_*`) |
| 4 | `0x0010` | Visibility/occlusion ветки |
| 5 | `0x0020` | Triangular remap в `sub_10005C60` |
| 6 | `0x0040` | Инверсия начального active-state |
| 7 | `0x0080` | Day/night filter (ветка A) |
| 8 | `0x0100` | Day/night filter (ветка B, инверсия) |
| 9 | `0x0200` | Alpha *= normalized lifetime |
| 10 | `0x0400` | Установка manager bit1 (`+0xA0`) |
| 11 | `0x0800` | Изменение gating в `sub_10007D10` |
| 12 | `0x1000` | Установка manager-state bit `0x10` |

Нерасшифрованные биты должны сохраняться 1:1.

### 3.4. `time_mode` (`0..17`)

Обозначения (`sub_10005C60`):

- `t0 = instance.start_ms`, `t1 = instance.end_ms`;
- `tn = (now_ms - t0) / (t1 - t0)`;
- `prev = instance.cached_alpha` (`v4+52` в дизассембле).

Режимы:

- `0`: constant (`instance.alpha_const`, поле `v4+40`);
- `1`: `tn`;
- `2`: `fract(tn)`;
- `3`: `1 - tn`;
- `4`: external value из queue/world API (manager `+36`, id из `this+104[a2]`);
- `5`: `|param33.xyz| / |param17.vecA.xyz|`;
- `6`: `param33.x / param17.vecA.x`;
- `7`: `param33.y / param17.vecA.y`;
- `8`: `param33.z / param17.vecA.z`;
- `9`: `|param36.xyz| / |param17.vecB.xyz|`;
- `10`: `param36.x / param17.vecB.x`;
- `11`: `param36.y / param17.vecB.y`;
- `12`: `param36.z / param17.vecB.z`;
- `13`: `1 - external_resource_value`;
- `14`: `1 - queue_param(49)`;
- `15`: `max(norm(param33/vecA), norm(param36/vecB))`;
- `16`: external (`mode 4`) с нижним clamp к `prev` (`0` не зажимается);
- `17`: external (`mode 4`) с верхним clamp к `prev` (`1` не зажимается).

Post-обработка после mode:

- если `flags & 0x200`: `alpha *= tn`;
- если `flags & 0x20`: triangular remap (`alpha = (alpha < 0.5 ? alpha : 1-alpha) * 2`).

---

## 4. Командный поток

### 4.1. Общий формат команды

Каждая команда:

- `uint32 cmd_word`;
- далее body фиксированного размера по opcode.

`cmd_word`:

- `opcode = cmd_word & 0xFF`;
- `enabled = (cmd_word >> 8) & 1`;
- `bits 9..31` в датасете нулевые, но их надо сохранять 1:1.

Выравнивания между командами нет.

### 4.2. Размеры

| Opcode | Размер записи |
|---:|---:|
| 1 | 224 |
| 2 | 148 |
| 3 | 200 |
| 4 | 204 |
| 5 | 112 |
| 6 | 4 |
| 7 | 208 |
| 8 | 248 |
| 9 | 208 |
| 10 | 208 |

### 4.3. Opcode -> runtime-класс (vtable)

| Opcode | `new(size)` | vtable |
|---:|---:|---|
| 1 | `0xF0` | `off_1001E78C` |
| 2 | `0xA0` | `off_1001F048` |
| 3 | `0xFC` | `off_1001E770` |
| 4 | `0x104` | `off_1001E754` |
| 5 | `0x54` | `off_1001E360` |
| 6 | `0x1C` | `off_1001E738` |
| 7 | `0x48` | `off_1001E228` |
| 8 | `0xAC` | `off_1001E71C` |
| 9 | `0x100` | `off_1001E700` |
| 10 | `0x48` | `off_1001E24C` |

### 4.4. Общий вызовной контракт команды

После создания команды (`sub_10007650`):

1. `cmd->enabled = cmd_word.bit8`.
2. `cmd->Init(fx_queue, fx_instance)` (`vfunc +4`).
3. команда добавляется в список инстанса.

В runtime cycle:

- `vfunc +8`: update/compute (bool);
- `vfunc +12`: emission/render callback;
- `vfunc +20`: toggle active;
- `vfunc +16`/`+24`: служебные функции (зависят от opcode).

---

## 5. Загрузка FXID (engine-accurate)

`sub_10007650`:

```c
void FxLoad(FxInstance* fx, uint8_t* payload) {
    FxHeader60* h = (FxHeader60*)payload;

    fx->raw_header = h;
    fx->mode = h->time_mode;
    fx->end_ms = fx->start_ms + h->duration_sec * 1000.0f;
    fx->scale = {h->scale_x, h->scale_y, h->scale_z};
    fx->active_default = ((h->flags & 0x40) == 0);

    uint8_t* ptr = payload + 0x3C;
    for (uint32_t i = 0; i < h->cmd_count; ++i) {
        uint32_t w = *(uint32_t*)ptr;
        uint8_t op = (uint8_t)(w & 0xFF);

        Command* cmd = CreateByOpcode(op, ptr); // может вернуть null
        if (cmd) {
            cmd->enabled = (w >> 8) & 1;

            if (h->flags & 0x400) fx->manager_flags |= 0x0100;
            if ((h->flags & 0x400) || cmd->enabled) fx->manager_flags |= 0x0010;

            cmd->Init(fx->queue, fx);
            fx->commands.push_back(cmd);
        }

        ptr += size_by_opcode(op); // без bounds checks в оригинале
    }
}
```

Критичные edge-case оригинала:

- bounds checks отсутствуют;
- при unknown opcode `ptr` не двигается (`advance = 0`);
- при `new == null` команда пропускается, но `ptr` двигается.

Фактический `advance` в `sub_10007650` задан hardcoded в DWORD:

- `op1:+56`, `op2:+37`, `op3:+50`, `op4:+51`, `op5:+28`,
- `op6:+1`, `op7:+52`, `op8:+62`, `op9:+52`, `op10:+52`,
- `default:+0`.

---

## 6. Runtime lifecycle

- `sub_10007470`: ctor instance.
- `sub_10003D30(case 28)`: per-frame update manager.
- `sub_10006170`: gate + alpha/time + command updates.
- `sub_10008120` / `sub_10007D10`: update/render branches.
- Start/Stop: `sub_10004BD0` / `sub_10004C10`.

Event-codes `sub_10003D30`:

- `4`: bootstrap/time init;
- `20`: range-removal + index repair;
- `23`: set manager bit0;
- `24`: clear manager bit0;
- `28`: main tick.

---

## 7. Общий тип `ResourceRef64`

Для opcode `2/3/4/5/7/8/9/10` присутствует ссылка вида:

```c
struct ResourceRef64 {
    char archive[32]; // null-terminated ASCII, case-insensitive compare
    char name[32];    // null-terminated ASCII
};
```

Поведение loader'а:

- оба имени обязаны быть непустыми;
- кэширование по `(_strcmpi archive, _strcmpi name)`;
- загрузка/резолв через manager resource API.

Наблюдение по данным:

- для `opcode 2`: обычно `sounds.lib` + `*.wav`;
- для остальных: обычно `material.lib` + material name.

---

## 8. Полная карта body по opcode (field-level)

Смещения указаны от начала команды (включая `cmd_word`).

### 8.1. Opcode 1 (`off_1001E78C`, size=224)

Основные методы:

- init: `sub_1000F4B0`;
- update: `sub_1000F6E0`;
- emit: `nullsub_2`;
- toggle: `sub_1000F490`.

```c
struct FxCmd01 {
    uint32_t word;                // +0
    uint32_t mode;                // +4  (enum, см. ниже)
    float    t_start;             // +8
    float    t_end;               // +12

    float    p0_min[3];           // +16..24
    float    p0_max[3];           // +28..36

    float    p1_min[3];           // +40..48
    float    p1_max[3];           // +52..60

    float    q0_min[4];           // +64..76
    float    q0_max[4];           // +80..92

    float    q0_rand_span[4];     // +96..108 (все 4 читаются в sub_1000F6E0)

    float    scalar_min;          // +112
    float    scalar_max;          // +116
    float    scalar_rand_amp;     // +120

    float    color_rgb[3];        // +124..132 (вызов manager+16)

    float    opaque_tail6[6];     // +136..156 (сохранять 1:1; в датасете почти всегда 0)

    char     opt_archive[32];     // +160..191 (редко, напр. "material.lib")
    char     opt_name[32];        // +192..223 (редко, напр. "light_w")
};
```

Замечания по полям op1:

- `+108` не резерв: участвует в random-выборке как 4-я компонента блока `+96..108`;
- `+136..156` не читается vtable-методами класса `off_1001E78C` в `Effect.dll` (init/update/toggle/accessor), но должно сохраняться 1:1;
- редкий кейс с ненулевыми `+136..156` и строками `+160/+192` зафиксирован в `effects.rlb:r_lightray_w`.

`mode` (`+4`) -> параметры вызова manager (`sub_1000F4B0`):

- `1 -> create_kind=1, flags=0x80000000`;
- `2/5 -> create_kind=1, flags=0x00000000`;
- `3 -> create_kind=3, flags=0x00000000`;
- `4 -> create_kind=4, flags=0x00000000`;
- `6 -> create_kind=1, flags=0xA0000000`;
- `7 -> create_kind=1, flags=0x20000000`.

### 8.2. Opcode 2 (`off_1001F048`, size=148)

Основные методы:

- init: `sub_10012D10`;
- update: `sub_10012EB0`;
- emit: `nullsub_2`;
- toggle: `sub_10013170`.

```c
struct FxCmd02 {
    uint32_t word;                // +0
    uint32_t mode;                // +4  (0..3; влияет на sub_100065A0 mapping)
    float    t_start;             // +8
    float    t_end;               // +12

    float    a_min[3];            // +16..24
    float    a_max[3];            // +28..36

    float    b_min[3];            // +40..48
    float    b_max[3];            // +52..60

    float    c0_base;             // +64
    float    c1_base;             // +68
    float    c2_base;             // +72
    float    c2_max;              // +76

    uint32_t param_910;           // +80 (передаётся в manager cmd=910)

    ResourceRef64 ref;            // +84..147 (обычно sounds.lib + wav)
};
```

`mode` -> внутренний map в `sub_100065A0`:

- `0 -> 0`, `1 -> 512`, `2 -> 2`, `3 -> 514`.

### 8.3. Opcode 3 (`off_1001E770`, size=200)

Методы:

- init: `sub_100103B0`;
- update: `sub_100105F0`;
- emit: `sub_100106C0`.

```c
struct FxCmd03 {
    uint32_t word;                // +0
    uint32_t mode;                // +4

    float    alpha_source;        // +8   (>=0: norm time, <0: global time)
    float    alpha_pow_a;         // +12
    float    alpha_pow_b;         // +16

    float    out_min;             // +20
    float    out_max;             // +24
    float    out_pow;             // +28

    float    active_t0;           // +32
    float    active_t1;           // +36

    float    v0_min[3];           // +40..48
    float    v0_max[3];           // +52..60

    float    pow0[3];             // +64..72

    float    v1_min[3];           // +76..84
    float    v1_max[3];           // +88..96

    float    v2_min[3];           // +100..108
    float    v2_max[3];           // +112..120

    float    pow1[3];             // +124..132

    ResourceRef64 ref;            // +136..199
};
```

### 8.4. Opcode 4 (`off_1001E754`, size=204)

Layout как opcode 3 + последний коэффициент:

```c
struct FxCmd04 {
    FxCmd03 base;                 // +0..199
    float   dist_norm_inv_base;   // +200 (используется в sub_100108C0/100109B0)
};
```

`sub_100108C0`: `obj->inv = 1.0 / raw[200]`.

### 8.5. Opcode 5 (`off_1001E360`, size=112)

Методы:

- init: `sub_100028A0`;
- update: `sub_10002A20`;
- emit: `sub_10002BE0`;
- context update: `sub_10003070`.

```c
struct FxCmd05 {
    uint32_t word;                // +0
    uint32_t mode;                // +4  (в данных обычно 1)
    uint32_t unused_08;           // +8  (в текущем коде opcode5 не читается)
    uint32_t unused_0C;           // +12 (в текущем коде opcode5 не читается)

    float    active_t0;           // +16
    uint32_t max_segments;        // +20
    float    active_t1_min;       // +24
    float    active_t1_max;       // +28

    float    step_norm;           // +32
    float    segment_len;         // +36
    float    alpha_source;        // +40 (>=0 norm, <0 random)
    float    alpha_pow;           // +44

    ResourceRef64 ref;            // +48..111
};
```

### 8.6. Opcode 6 (`off_1001E738`, size=4)

Только `cmd_word`:

```c
struct FxCmd06 {
    uint32_t word; // +0
};
```

`init/update/emit` фактически no-op (`sub_100030B0` возвращает `0`).

### 8.7. Opcode 7 (`off_1001E228`, size=208)

Методы:

- init: `sub_10001720`;
- update: `sub_10001230`;
- emit: `sub_10001300`;
- element accessor: `sub_10002780`.

```c
struct FxCmd07 {
    uint32_t word;                // +0
    uint32_t mode;                // +4

    float    eval_min;            // +8
    float    eval_max;            // +12
    float    eval_pow;            // +16

    float    active_t0;           // +20
    float    active_t1;           // +24

    float    phase_span;          // +28
    float    phase_rate;          // +32

    uint32_t count_a;             // +36
    uint32_t count_b;             // +40

    float    set0_min[3];         // +44..52
    float    set0_max[3];         // +56..64
    float    set0_rand[3];        // +68..76
    float    set0_pow[3];         // +80..88

    float    set1_min[3];         // +92..100
    float    set1_max[3];         // +104..112
    float    set1_rand[3];        // +116..124
    float    set1_pow[3];         // +128..136

    float    gravity_or_drag_k;   // +140

    ResourceRef64 ref;            // +144..207
};
```

### 8.8. Opcode 8 (`off_1001E71C`, size=248)

Методы:

- init: `sub_10011230`;
- update: `sub_100115C0`;
- emit: `sub_10012030`.

```c
struct FxCmd08 {
    uint32_t word;                // +0
    uint32_t mode;                // +4

    float    eval_t0;             // +8
    float    eval_t1;             // +12

    float    gate_t0;             // +16
    float    gate_t1;             // +20

    float    period_min;          // +24
    float    period_max;          // +28
    float    phase_pow;           // +32

    uint32_t slots;               // +36

    float    set0_min[3];         // +40..48
    float    set0_max[3];         // +52..60
    float    set0_rand[3];        // +64..72

    float    set1_min[3];         // +76..84
    float    set1_max[3];         // +88..96
    float    set1_rand[3];        // +100..108

    float    set2_rand[3];        // +112..120
    float    set2_pow[3];         // +124..132

    float    rmax_set0[3];        // +136..144 (bound/radius calc)
    float    rmax_set1[3];        // +148..156 (bound/radius calc)
    float    rmax_set2[3];        // +160..168 (bound/radius calc)

    float    render_pow[3];       // +172..180

    ResourceRef64 ref;            // +184..247
};
```

### 8.9. Opcode 9 (`off_1001E700`, size=208)

Layout как opcode 3 с двумя final-полями:

```c
struct FxCmd09 {
    FxCmd03 base;                 // +0..199
    uint32_t render_kind;         // +200 (0/1/2 -> 3/5/6 in sub_100138C0)
    uint32_t render_flag;         // +204 (0 -> добавляет bit 0x08000000)
};
```

Методы:

- init/update как у opcode 3 (`sub_100103B0`, `sub_100105F0`);
- emit: `sub_100138C0` -> формирует код рендера и вызывает `sub_100106C0`.

### 8.10. Opcode 10 (`off_1001E24C`, size=208)

Body-layout совпадает с opcode 7 (`FxCmd07`), но другой runtime класс.

- init: `sub_10001A40`;
- update: `sub_10001230`;
- emit: `sub_10001300`;
- element accessor: `sub_10002830`.

Наблюдение по данным:

- `mode` (`+4`) встречается как `16` или `32`.

---

## 9. Runtime-специфика по opcode (важные отличия)

### 9.1. Opcode 1

- создаёт handle через manager (`vfunc +48`);
- задаёт флаги handle (`vfunc +52`);
- в update пушит:
  - позиционный вектор 1 (`vfunc +32`),
  - позиционный вектор 2 (`vfunc +36`),
  - 4-компонентный параметр (`vfunc +12`),
  - scalar+rgb (`vfunc +16`).

### 9.2. Opcode 2

- `ResourceRef64` резолвится через `sub_100065A0` (режим-зависимая загрузка, в данных обычно `sounds.lib`/`wav`);
- использует manager-команду id `910`.

### 9.3. Opcode 3/4/9

- общий core-emitter в `sub_100106C0`;
- opcode 4 добавляет нормализацию по `raw+200`;
- opcode 9 добавляет переключение render-кода (`raw+200/+204`).

### 9.4. Opcode 5

- держит массив внутренних сегментов (`332` байта/элемент, ctor `sub_100099F0`);
- context-matrix приходит через `vfunc +24` (`sub_10003070`).

### 9.5. Opcode 7/10

- общий update/render (`sub_10001230`, `sub_10001300`);
- разные внутренние element-форматы:
  - opcode 7: `204` байта/элемент (`sub_100092D0`),
  - opcode 10: `492` байта/элемент (`sub_1000BB40`).

### 9.6. Opcode 8

- самый тяжёлый спавнер, хранит ring/slot-структуры;
- emit фаза (`sub_10012030`) использует `mode`, `render_pow`, per-slot transforms.

---

## 10. Спецификация инструментов

### 10.1. Reader (strict)

Алгоритм:

1. `len(payload) >= 60`;
2. читаем `cmd_count`;
3. `ptr = 0x3C`;
4. цикл `cmd_count`:
   - `ptr + 4 <= len`;
   - `opcode in 1..10`;
   - `ptr + size(opcode) <= len`;
   - `ptr += size(opcode)`;
5. strict-tail: `ptr == len(payload)`.

### 10.2. Reader (engine-compatible)

Legacy-режим (опасный, только при необходимости byte-совместимости):

- без bounds-check;
- tolerant к unknown opcode как в оригинале.

### 10.3. Writer (canonical)

1. записать `FxHeader60`;
2. `cmd_count = commands.len()`;
3. команды сериализуются как `cmd_word + fixed-body`;
4. размер payload: `0x3C + sum(size(op_i))`;
5. без хвостовых байт.

### 10.4. Editor (lossless)

Правила:

- все поля little-endian;
- не менять fixed size команды;
- не добавлять padding;
- сохранять неизвестные биты (`cmd_word`, `header.flags`) copy-through;
- для частично-известных полей поддерживать режим `opaque`.

### 10.5. IR/JSON (рекомендуемая форма)

```json
{
  "header": {
    "time_mode": 1,
    "duration_sec": 2.5,
    "phase_jitter": 0.2,
    "flags": 22,
    "settings_id": 785,
    "rand_shift": [0.0, 0.0, 0.0],
    "pivot": [0.0, 0.0, 0.0],
    "scale": [1.0, 1.0, 1.0]
  },
  "commands": [
    {
      "opcode": 8,
      "word_raw": 264,
      "enabled": 1,
      "fields": {
        "mode": 1065353216,
        "eval_t0": 0.0,
        "eval_t1": 1.0,
        "resource": {"archive": "material.lib", "name": "fire_smoke"}
      },
      "opaque_extra_hex": "..."
    }
  ]
}
```

---

## 11. Проверка на реальных данных

`testdata/nres`:

- FXID payload: `923`;
- валидация parser'а: `923/923 valid`.

Распределение opcode:

- `1: 618`
- `2: 517`
- `3: 1545`
- `4: 202`
- `5: 31`
- `6: 0` (в датасете не встречен, но поддержан)
- `7: 1161`
- `8: 237`
- `9: 266`
- `10: 160`

Подтверждённые `ResourceRef64` оффсеты:

- op2 `+84`, op3/4/9 `+136`, op5 `+48`, op7/10 `+144`, op8 `+184`.

Для op1 найден редкий расширенный хвост (`+160/+192`) в `effects.rlb:r_lightray_w`:

- `material.lib` / `light_w`.

---

## 12. Практический чек-лист 1:1

Для runtime-порта:

- реализовать `FxHeader60` и parser `sub_10007650`;
- реализовать opcode-классы с методами как в vtable;
- учитывать start/stop/restart контракт manager API;
- воспроизвести `sub_10005C60` + post-flags (`0x20`, `0x200`);
- воспроизвести event loop `sub_10003D30(case 28)`.

Для toolchain:

- strict validator по разделу 10.1;
- canonical writer по разделу 10.3;
- field-aware editor + opaque fallback для неизвестных зон.

---

## 13. Что считать «полной» совместимостью

Практический критерий завершения:

1. Парсер и writer дают byte-identical round-trip для всех 923 FXID.
2. Runtime-порт выдаёт совпадающие state transitions на одинаковом `dt/seed` (по ключевым полям instance + command state).
3. Все opcode `1..10` поддержаны (включая `6`, даже если отсутствует в текущем датасете).
4. `ResourceRef64` и mode-ветки (`op1`, `op2`, `op9`) совпадают с оригиналом.

Эта страница покрывает весь наблюдаемый контракт формата/рантайма и полную карту body-полей по всем opcode.

---

## 14. Что осталось до «абсолютных 100%»

Для практического 1:1 (парсер/writer/runtime на известном контенте) покрытие уже достаточно.  
Для «абсолютных 100%» на любых входах и во всех краевых режимах остаются 3 пункта:

1. FP-детерминизм: оригинал опирается на x87-style вычисления; SSE/fast-math могут давать расхождения в alpha/таймингах.
2. RNG parity: используется `sub_10002220` (16-bit генератор) и глобальные seed-состояния; для bit-exact воспроизведения нужны контрольные трассы оригинала.
3. Редкие ветки данных: в текущем датасете нет opcode `6`, и почти не встречаются хвосты op1 (`+136..223`); для исчерпывающей валидации нужны дополнительные FXID-образцы.

Что нужно собрать, чтобы закрыть это полностью:

- frame-by-frame dump из оригинального runtime (alpha, manager flags, per-command state);
- контрольные прогоны при фиксированном `dt` и seed;
- минимум по одному ресурсу на каждую редкую ветку (`op6`, op1-tail с ненулевыми `+136..223`).
