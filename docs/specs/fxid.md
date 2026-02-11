# FXID

Документ описывает формат ресурса эффекта `FXID`, контракт runtime в `Effect.dll` и практические правила для инструментов чтения/конвертации/редактирования.

Цель: дать достаточную high-level спецификацию для:

- 1:1 загрузчика/рантайма эффекта;
- валидатора payload;
- бинарно-совместимого редактора;
- конвертера в промежуточный формат и обратно.

Связанный контейнер: [NRes / RsLi](nres.md).

---

## 1. Источники восстановления

Спецификация собрана по:

- `tmp/disassembler1/Effect.dll.c`;
- `tmp/disassembler2/Effect.dll.asm`;
- интеграционным вызовам из `tmp/disassembler1/Terrain.dll.c`;
- проверке реальных архивов `testdata/nres`.

Ключевые точки:

- parser FXID: `Effect.dll!sub_10007650`;
- core update: `Effect.dll!sub_10008120`, `sub_10006170`, `sub_10007D10`;
- export API: `CreateFxManager`, `InitializeSettings`.

---

## 2. Место формата в движке

### 2.1. Контейнер NRes

Эффект хранится как запись NRes с типом:

- `type_id = 0x44495846` (`"FXID"`).

Для всех 923 FXID-entries в `testdata/nres` подтверждено:

- `attr1 = 0`;
- `attr2 = 0`;
- `attr3 = 1`.

### 2.2. Runtime-модуль

`Effect.dll` экспортирует 2 функции:

- `CreateFxManager(int a1, int a2, int owner)`;
- `InitializeSettings()`.

`CreateFxManager` выделяет объект (`0xB8` байт), инициализирует его через `sub_10003AE0`, возвращает **интерфейсный указатель** (смещение `+4` от базового объекта).

### 2.3. COM-подобный интерфейс

Внешний код (например, `Terrain.dll`) получает рабочий интерфейс через `QueryInterface(id=19)` и далее вызывает методы vtable `off_1001E478`.

Ключевые методы интерфейса менеджера (по vtable):

| Vtable offset | Функция            | Назначение (high-level) |
|---|---|---|
| +0x10 | `sub_10004320` | Открыть/закэшировать ресурс эффекта (`archive + name`) |
| +0x14 | `sub_10004590` | Создать runtime-инстанс эффекта по шаблону |
| +0x18 | `sub_10004780` | Удалить инстанс по id |
| +0x1C | `sub_100047B0` | Установить режим интерполяции/времени |
| +0x20 | `sub_100047D0` | Установить scale |
| +0x24 | `sub_10004830` | Установить позицию |
| +0x28 | `sub_10004930` | Установить матрицу transform |
| +0x2C | `sub_10004B00` | Перезапуск с mode |
| +0x38 | `sub_10004BA0` | Модификатор длительности |
| +0x3C | `sub_10004BD0` | Start/Enable |
| +0x40 | `sub_10004C10` | Stop/Disable |
| +0x44 | `sub_10004C50` | Привязать emitter/context |
| +0x48 | `sub_10004D50` | Сброс frame-флагов |
| +0x08 | `sub_10003D30` | Системные event-коды (tick/reset/remove-range) |

Этого контракта достаточно, чтобы корректно встроить FXID-рантайм в движок.

---

## 3. Бинарный формат payload FXID

Все числа little-endian.

## 3.1. Header (60 байт, `0x3C`)

```c
struct FxHeader60 {
    uint32_t cmd_count;        // 0x00: число команд
    uint32_t time_mode;        // 0x04: базовый режим вычисления alpha/time
    float    duration_sec;     // 0x08: длительность эффекта в секундах
    float    phase_jitter;     // 0x0C: амплитуда рандом-сдвига alpha (если flags bit0)
    uint32_t flags;            // 0x10: флаги runtime (см. таблицу ниже)
    uint32_t settings_id;      // 0x14: id категории/настройки (используется low8)
    float    rand_shift_x;     // 0x18: рандомный сдвиг (если flags bit3)
    float    rand_shift_y;     // 0x1C
    float    rand_shift_z;     // 0x20
    float    pivot_x;          // 0x24: опорная точка/anchor
    float    pivot_y;          // 0x28
    float    pivot_z;          // 0x2C
    float    scale_x;          // 0x30: базовый scale
    float    scale_y;          // 0x34
    float    scale_z;          // 0x38
};
```

Командный поток начинается строго с `offset = 0x3C`.

## 3.2. Поля header: подтверждённая семантика

- `cmd_count`:
  - engine итерируется ровно `cmd_count` раз;
  - дополнительных ограничений в оригинале нет.
- `time_mode`:
  - начальный runtime-mode (`effect+0x14`), участвует в `sub_10005C60`.
- `duration_sec`:
  - переводится в миллисекунды как `duration_ms = duration_sec * 1000.0`.
- `phase_jitter`:
  - при `flags & 0x1` к вычисленному alpha добавляется рандом в диапазоне `[-phase_jitter/2, +phase_jitter/2]`.
- `settings_id`:
  - `sub_1000EC40` использует только `settings_id & 0xFF` как индекс таблицы настроек.
- `rand_shift_*`:
  - при `flags & 0x8` добавляется рандомный сдвиг к позиции эффекта.
- `pivot_*`:
  - используется как опорная точка в ветках проверки видимости/окклюзии (`sub_10007D10`).
- `scale_*`:
  - копируется в runtime (`this+56..64`) и участвует в построении матрицы в `sub_10007C90`.

## 3.3. `flags` (`header+0x10`) — подтвержденные биты

| Бит | Маска | Поведение |
|---|---:|---|
| 0 | `0x0001` | Включает random phase jitter (`phase_jitter`) |
| 3 | `0x0008` | Включает random positional shift (`rand_shift_*`) |
| 4 | `0x0010` | Участвует в ветках видимости/окклюзии в `sub_10006170`/`sub_10007D10` |
| 5 | `0x0020` | Треугольная ремап-функция alpha в `sub_10005C60` |
| 6 | `0x0040` | Инвертирует начальное активное состояние (`this+324 = !(flags&0x40)`) |
| 7 | `0x0080` | Условная фильтрация по manager-флагу day/night |
| 8 | `0x0100` | Инверсная day/night фильтрация |
| 9 | `0x0200` | Домножение alpha на нормализованное время жизни |
| 10 | `0x0400` | Включает manager-глобальный флаг (`manager+0xA0` bit1) |
| 11 | `0x0800` | Меняет поведение ветки `sub_10007D10` (gating для checks) |
| 12 | `0x1000` | Проставляет manager-state bit0x10 в `sub_10006170` |

Остальные биты в движке напрямую не расшифрованы на уровне high-level, но должны сохраняться 1:1.

## 3.4. `time_mode` (`header+0x04`) — режимы `sub_10005C60`

Поддерживаются коды `0..17`.

| mode | Логика |
|---:|---|
| 0 | Константа (значение из runtime-поля) |
| 1 | Линейно: `(t - t0) / (t1 - t0)` |
| 2 | Цикл `frac((t - t0)/(t1 - t0))` |
| 3 | Обратная линейная: `1 - (t - t0)/(t1 - t0)` |
| 4 | Значение из внешнего queue/world-запроса |
| 5..8 | Нормированные отношения компонент вектора (camera/world path) |
| 9..12 | Альтернативный набор нормированных отношений |
| 13 | `1 - value` из queue-запроса по объекту |
| 14 | `1 - value` из параметра queue id=49 |
| 15 | max из двух нормированных длин |
| 16 | Кламп "не убывать" относительно предыдущего значения |
| 17 | Кламп "не возрастать" относительно предыдущего значения |

После базового mode-преобразования применяются post-флаги `0x200` и `0x20`.

---

## 4. Командный поток

## 4.1. Формат записи команды

Каждая команда начинается с `uint32 cmd_word`.

Биты:

- `opcode = cmd_word & 0xFF`;
- `enabled = (cmd_word >> 8) & 1`;
- в реальных данных `bits 9..31 == 0` (но редактор должен сохранять весь word как есть).

Никакого межкомандного выравнивания нет: следующая команда начинается сразу после `size(opcode)`.

## 4.2. Размеры записей по opcode

| Opcode | Размер записи (байт) | Размер тела после `cmd_word` |
|---:|---:|---:|
| 1  | 224 | 220 |
| 2  | 148 | 144 |
| 3  | 200 | 196 |
| 4  | 204 | 200 |
| 5  | 112 | 108 |
| 6  | 4   | 0 |
| 7  | 208 | 204 |
| 8  | 248 | 244 |
| 9  | 208 | 204 |
| 10 | 208 | 204 |

## 4.3. Opcode -> runtime-класс

В `sub_10007650` для opcode создаются объекты:

| Opcode | `operator new` | Runtime vtable |
|---:|---:|---|
| 1  | `0xF0`  | `off_1001E78C` |
| 2  | `0xA0`  | `off_1001F048` |
| 3  | `0xFC`  | `off_1001E770` |
| 4  | `0x104` | `off_1001E754` |
| 5  | `0x54`  | `off_1001E360` |
| 6  | `0x1C`  | `off_1001E738` |
| 7  | `0x48`  | `off_1001E228` |
| 8  | `0xAC`  | `off_1001E71C` |
| 9  | `0x100` | `off_1001E700` |
| 10 | `0x48`  | `off_1001E24C` |

Важно: payload команды хранится как сырой указатель и разбирается runtime-методами класса.

## 4.4. Внутренний вызовной контракт команд

После создания каждой команды менеджер:

1. Проставляет `enabled` из `cmd_word.bit8` в поле `obj+4`.
2. Вызывает инициализацию команды (`vfunc +4`) с аргументами `(queue, manager)`.
3. Добавляет команду в массив команд эффекта.

В update-cycle менеджер вызывает:

- `vfunc +8`: вычисление/обновление команды (bool);
- `vfunc +12`: callback при render/emission;
- `vfunc +20`: toggle активности;
- `vfunc +24`: обновление transform-context (для части opcode no-op).

---

## 5. Алгоритм загрузки FXID (engine-accurate)

Псевдокод `sub_10007650`:

```c
void FxLoad(FxInstance* fx, uint8_t* payload) {
    FxHeader60* h = (FxHeader60*)payload;

    fx->raw_header_ptr = h;
    fx->mode = h->time_mode;
    fx->end_ms = h->duration_sec * 1000.0f + fx->start_ms;
    fx->scale = { h->scale_x, h->scale_y, h->scale_z };
    fx->active_default = ((h->flags & 0x40) == 0);

    uint8_t* ptr = payload + 0x3C;

    for (uint32_t i = 0; i < h->cmd_count; i++) {
        uint32_t w = *(uint32_t*)ptr;
        uint8_t op = (uint8_t)(w & 0xFF);

        Command* cmd = CreateCommandByOpcode(op, ptr); // может вернуть null

        if (cmd != null) {
            cmd->enabled = (w >> 8) & 1;

            if (h->flags & 0x400)
                fx->manager_flags |= 0x0100; // внутренний bit

            if ((h->flags & 0x400) || cmd->enabled)
                fx->manager_flags |= 0x0010;

            cmd->Attach(fx->queue, fx);
            fx->commands.push_back(cmd);
        }

        ptr += size_by_opcode(op); // в оригинале без checks
    }
}
```

Поведение оригинала, важное для 1:1:

- проверок границ буфера нет;
- при `unknown opcode` указатель `ptr` не двигается (счётчик цикла движется);
- при `new == null` команда пропускается, но `ptr` двигается на размер opcode.

Для toolchain рекомендуется **строгий** и **безопасный** парсер (см. раздел 7).

---

## 6. Runtime-жизненный цикл эффекта

## 6.1. Инициализация

- `sub_10007470`: конструктор instance;
- инициализируются матрицы/scale/флаги;
- начальный `mode` берётся из header.

## 6.2. Tick и обновление

Основной тик идёт через `sub_10003D30(case 28)`:

1. обновление времени manager;
2. обход активных FX instances;
3. для каждого инстанса `sub_10006170`:
   - gating по `flags`/queue-state;
   - вычисление alpha через `sub_10005C60`;
   - вызов `sub_10008120` (update/bounds/command-pass);
   - при необходимости `sub_10007D10` (эмиссия/рендерный callback).

## 6.3. Start/Stop/Restart API

- Start: `sub_10004BD0` -> `sub_10007A30(..., 1, now)`;
- Stop: `sub_10004C10` -> `sub_10007A30(..., 0, now)`;
- Restart/retime: `sub_10004B00`, `sub_10004BA0`.

## 6.4. Manager event-codes (`sub_10003D30`)

Обработанные коды:

- `4`: bootstrap + установка текущего времени;
- `20`: удаление диапазона объектов в queue и корректировка индексов;
- `23`: выставить manager-flag bit0;
- `24`: сбросить manager-flag bit0;
- `28`: основной per-frame update.

---

## 7. Спецификация для инструментов

## 7.1. Reader (strict)

Рекомендуемый строгий парсер:

1. проверить `len(payload) >= 60`;
2. прочитать `cmd_count`;
3. `ptr = 0x3C`;
4. для каждой команды:
   - требовать `ptr + 4 <= len`;
   - прочитать `opcode`;
   - `opcode` должен быть в `1..10`;
   - `ptr + size(opcode) <= len`;
   - `ptr += size(opcode)`;
5. в strict-режиме требовать `ptr == len(payload)`.

Такой алгоритм совпадает с валидатором `tools/msh_doc_validator.py`.

## 7.2. Reader (engine-compatible)

Для byte-level совместимости с оригиналом можно поддержать legacy-режим:

- без bounds-check (как `Effect.dll`);
- с toleration на `unknown opcode` (но это потенциально unsafe).

## 7.3. Editor (без потери совместимости)

Безопасные операции:

- менять `header`-поля (mode, duration, flags, scale, pivot);
- менять `enabled` через `cmd_word.bit8`;
- удалять/вставлять команды с корректным пересчётом `cmd_count` и сдвигом stream;
- сохранять command-body как opaque bytes, если нет полного field-level декодера.

Правила:

- всегда little-endian;
- не менять размеры записей opcode;
- не вставлять padding между командами;
- для неизвестных битов `cmd_word` и `header.flags` использовать copy-through.

## 7.4. Writer (canonical)

Каноническая сборка payload:

1. записать `FxHeader60`;
2. `cmd_count = len(commands)`;
3. для каждой команды записать `cmd_word` + body фиксированного размера для opcode;
4. итоговый размер должен быть `0x3C + sum(size(opcode_i))`;
5. без хвоста.

## 7.5. Конвертация в промежуточный JSON

Рекомендуемая структура для round-trip:

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
      "opcode": 3,
      "enabled": 1,
      "word_raw": 259,
      "body_hex": "..."
    }
  ]
}
```

`body_hex` хранит opaque payload без потери данных.

---

## 8. Проверка на реальных данных

`testdata/nres` (через `tools/msh_doc_validator.py`) :

- FXID effects: `923/923 valid`.

Дополнительно по этим 923 payload:

- `cmd_count`: min `0`, max `81`, avg `5.13`;
- `duration_sec`: min `0.0`, max `60.0`, avg `2.46`;
- `opcode` распределение:
  - `1: 618`
  - `2: 517`
  - `3: 1545`
  - `4: 202`
  - `5: 31`
  - `7: 1161`
  - `8: 237`
  - `9: 266`
  - `10: 160`
  - `6`: не встречен, но поддержан parser.
- `cmd_word`:
  - `bits 9..31` не использованы в датасете;
  - `bit8` встречается для части opcode (особенно `3`, `7`, `9`).

---

## 9. Известные пробелы (не блокируют 1:1 container/runtime)

1. Полная человеко-читаемая семантика **внутренних полей command body** для каждого opcode не завершена.
2. Для части битов `header.flags` есть только functional-наблюдение без финального gameplay-имени.
3. Высокие биты `settings_id` используются как есть (runtime читает low8); их предметное имя не зафиксировано.

Это не мешает:

- корректно читать/валидировать/пересобирать FXID;
- делать lossless редактирование;
- воспроизводить lifecycle менеджера и update-loop 1:1 на уровне контракта.

---

## 10. Минимальный чек-лист реализации

Для 1:1-порта движка:

- реализовать `FxHeader60` и stream parser по размерам opcode;
- реализовать менеджер API (раздел 2.3);
- реализовать tick-path `03D30(case 28)` -> `06170` -> `08120`/`07D10`;
- учитывать флаги `0x40`, `0x400`, `0x800`, `0x1000`, `0x80/0x100`, `0x20`, `0x200`.

Для инструментов:

- strict validator по разделу 7.1;
- canonical writer по разделу 7.4;
- opaque-представление command-body для безопасного round-trip.
