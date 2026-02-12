# MSH animation

Документ фиксирует анимационную часть формата MSH (`Res8`, `Res19`) и runtime-алгоритм сэмплирования/смешивания, необходимый для 1:1 совместимого движка и toolchain (reader/writer/converter/editor).

Связанные документы:
- [MSH core](msh-core.md) — общая структура модели и `Res1`/`Res2`.
- [NRes / RsLi](nres.md) — контейнер и атрибуты записей.

---

## 1. Область и источники

Спецификация основана на:
- `tmp/disassembler1/AniMesh.dll.c` (псевдо-C): `sub_10015FD0`, `sub_10012880`, `sub_10012560`.
- `tmp/disassembler2/AniMesh.dll.asm` (ASM): подтверждение x87-пути (`FISTP`) и ветвлений.
- `tmp/disassembler1/Ngi32.dll.c` (псевдо-C): `sub_10002F90`, `sub_10014540`, `sub_10014630`, `sub_10015D80`, `sub_10017E60`, `sub_10017F50`, `sub_10006D00`, `niGetProcAddress`.
- `tmp/disassembler2/Ngi32.dll.asm` (ASM): подтверждение таблицы `g_FastProc` и FPU control-word setup.
- валидации corpus (`testdata`): 435 моделей `*.msh`.

Ниже разделено на:
- **Нормативно**: обязательно для runtime-совместимости.
- **Канонично**: как устроены исходные ассеты; важно для детерминированного writer/editor.

---

## 2. Ресурсы и поля модели

### 2.1. Res8 — key pool (нормативно)

`Res8` — массив ключей фиксированного шага 24 байта.

```c
struct AnimKey24 {
    float   pos_x;   // +0x00
    float   pos_y;   // +0x04
    float   pos_z;   // +0x08
    float   time;    // +0x0C
    int16_t qx;      // +0x10
    int16_t qy;      // +0x12
    int16_t qz;      // +0x14
    int16_t qw;      // +0x16
};
```

Декодирование quaternion-компонент:

```c
float q = (float)s16 * (1.0f / 32767.0f);
```

Атрибуты NRes:
- `attr1 = size / 24` (количество ключей).
- `attr2 = 0` (в observed corpus).
- `attr3 = 4` (не stride; это фактический runtime-инвариант формата).

### 2.2. Res19 — frame->segment map (нормативно)

`Res19` — непрерывный `uint16` массив:

```c
uint16_t map_words[]; // count = size / 2
```

Атрибуты NRes:
- `attr1 = size / 2` (число `uint16` слов).
- `attr2 = animFrameCount` (глобальная длина таймлайна модели в кадрах).
- `attr3 = 2`.

### 2.3. Связь с Res1 node header (нормативно)

Для `Res1` со stride 38 (основной формат):
- `hdr2` (`node + 0x04`) = `mapStart` (`0xFFFF` => map для узла отсутствует).
- `hdr3` (`node + 0x06`) = `fallbackKeyIndex` (индекс ключа в `Res8`).

Runtime читает эти поля напрямую в `sub_10012880`.

### 2.4. Поля runtime-модели, задействованные анимацией (нормативно)

Инициализация в `sub_10015FD0`:
- `model+0x18` -> `Res8` pointer.
- `model+0x1C` -> `Res19` pointer.
- `model+0x9C` <- `NResEntry(Res19).attr2` (`animFrameCount`).

---

## 3. Runtime-сэмплирование узла (`sub_10012880`)

Функция возвращает:
- quaternion (4 float) в буфер `outQuat`,
- позицию (3 float) в `outPos`.

Вход:
- `t` — sample time.
- текущий `nodeIndex` берётся из runtime-объекта (не из аргумента).

### 3.1. Вычисление frame index (нормативно)

Алгоритм:
1. `x = t - 0.5`.
2. `frame = x87 FISTP(x)` (через 64-битный промежуточный буфер).

Важно:
- это не «просто floor»;
- поведение зависит от x87 control word.

В оригинальном runtime control word приводится к каноничному виду в `Ngi32::sub_10006D00`:
- `cw = (cw & 0xF0FF) | 0x003F`;
- это даёт `round-to-nearest` (RC=00), precision control `PC=00` и маскирование x87-исключений.

Если нужен byte/behavior 1:1, надо повторить именно x87-ветку или её точный эквивалент.

### 3.2. Выбор `keyIndex` (нормативно)

```c
node = Res1 + nodeIndex * 38;
mapStart  = u16(node + 4); // hdr2
fallback  = u16(node + 6); // hdr3

if ((uint32_t)frame >= animFrameCount
    || mapStart == 0xFFFF
    || map_words[mapStart + (uint32_t)frame] >= fallback) {
    keyIndex = fallback;
} else {
    keyIndex = map_words[mapStart + (uint32_t)frame];
}
```

Критично:
- runtime не проверяет bounds у `fallback` и `mapStart + frame`; некорректные данные приводят к OOB.

### 3.3. Сэмплирование ключей (нормативно)

`k0 = Res8[keyIndex]`.

Ветки:
1. fallback-ветка из п.3.2: возвращается строго `k0` (без `k1`).
2. map-ветка:
   - если `t == k0.time` -> вернуть `k0`;
   - иначе берётся `k1 = Res8[keyIndex + 1]`;
   - если `t == k1.time` -> вернуть `k1`;
   - иначе:
     - `alpha = (t - k0.time) / (k1.time - k0.time)`;
     - `pos = lerp(k0.pos, k1.pos, alpha)`;
     - `quat = fastproc_interp(k0.quat, k1.quat, alpha)` (`g_FastProc[17]`).

Сравнение `t == key.time` строгое (битовая float-эквивалентность по FPU compare), без epsilon.

### 3.4. Порядок quaternion-компонент в runtime (нормативно)

В `Res8` компоненты лежат как `qx,qy,qz,qw`, но в runtime-буферы они попадают в порядке:
- `outQuat[0] = qw`;
- `outQuat[1] = qx`;
- `outQuat[2] = qy`;
- `outQuat[3] = qz`.

То есть все `g_FastProc`-пути в анимации работают с quaternion в порядке `float4 = [w, x, y, z]`.

---

## 4. Runtime-смешивание двух сэмплов (`sub_10012560`)

`sub_10012560(this, tA, tB, blend, outMatrix4x4)` смешивает две позы.

### 4.1. Валидация входов (нормативно)

Выбор доступных сэмплов:
- `hasA = (blend < 1.0f) && (tA >= 0.0f)`.
- `hasB = (blend > 0.0f) && (tB >= 0.0f)`.

Ветки:
- только `hasA`: матрица из A.
- только `hasB`: матрица из B.
- оба: полноценное смешивание.
- ни одного: в оригинале путь не защищён (caller contract).

### 4.2. Смешивание quaternion (нормативно)

Перед интерполяцией выполняется shortest-path flip:

```c
if (|qA + qB|^2 < |qA - qB|^2) {
    qB = -qB;
}
```

Далее:
- `q = fastproc_blend(qA, qB, blend)` (`g_FastProc[22]`);
- `outMatrix = quat_to_matrix(q)` (`g_FastProc[14]`).

### 4.3. Смешивание translation (нормативно)

Позиция смешивается отдельно:

```c
pos = (1-blend) * posA + blend * posB;
outMatrix[3]  = pos.x;
outMatrix[7]  = pos.y;
outMatrix[11] = pos.z;
```

(`sub_1000B8E0` подтверждает, что используются именно эти ячейки).

### 4.4. Точные `g_FastProc[14/17/22]` (нормативно)

`niGetProcAddress(i)` в `Ngi32` возвращает `g_FastProc[i]` (таблица function pointers).
В `AniMesh` используются:
- `call [g_FastProc + 0x38]` -> index 14 -> `quat_to_matrix`.
- `call [g_FastProc + 0x44]` -> index 17 -> `quat_interp`.
- `call [g_FastProc + 0x58]` -> index 22 -> `quat_blend`.

Связь с символами `Ngi32` (по адресам таблицы):
- `g_FastProc` base = `0x1003A058`;
- index 14 -> `0x1003A090`;
- index 17 -> `0x1003A09C`;
- index 22 -> `0x1003A0B0`.

Назначения по CPU-веткам (`sub_10002F90`) и семантика:
- scalar path: `14=sub_10017E60` (или `sub_10014540`), `17=22=sub_10017F50` (или `sub_10014630`);
- SIMD path (`dword_1003A168`): `14=sub_1001D830`, `17=22=sub_10015D80`;
- все варианты эквивалентны по математике.

Точная формула `quat_to_matrix` для `q=[w,x,y,z]`:

```c
m[0]  = 1 - 2*(y*y + z*z);
m[1]  = 2*(x*y + w*z);
m[2]  = 2*(x*z - w*y);
m[3]  = 0;

m[4]  = 2*(x*y - w*z);
m[5]  = 1 - 2*(x*x + z*z);
m[6]  = 2*(y*z + w*x);
m[7]  = 0;

m[8]  = 2*(x*z + w*y);
m[9]  = 2*(y*z - w*x);
m[10] = 1 - 2*(x*x + y*y);
m[11] = 0;

m[12] = 0;
m[13] = 0;
m[14] = 0;
m[15] = 1;
```

Точная формула `quat_interp`/`quat_blend` (`index 17` и `22`, один и тот же алгоритм):

```c
float dot = dot4(q0, q1);
float sign = 1.0f;
if (dot < 0.0f) { dot = -dot; sign = -1.0f; }

float w0, w1;
if (1.0f - dot <= 9.9999997e-6f) {
    w0 = 1.0f - a;
    w1 = a;
} else {
    float theta = acos(dot);
    float inv_sin_theta = 1.0f / sin(theta);
    w1 = sin(a * theta) * inv_sin_theta;
    w0 = cos(a * theta) - w1 * dot;
}
w1 *= sign;
out = w0 * q0 + w1 * q1;
```

Примечание: явной нормализации `out` в конце нет; используется закрытая форма SLERP-весов.

Reference pseudocode:

```c
void blend_pose(Model *m, float tA, float tB, float blend, float out_m[16]) {
    bool hasA = (blend < 1.0f) && (tA >= 0.0f);
    bool hasB = (blend > 0.0f) && (tB >= 0.0f);

    float qA[4], qB[4], pA[3], pB[3];
    if (hasA) sample_node_pose(m, m->node_index, tA, qA, pA);
    if (hasB) sample_node_pose(m, m->node_index, tB, qB, pB);

    if (hasA && !hasB) { quat_to_matrix(qA, out_m); set_translation(out_m, pA); return; }
    if (!hasA && hasB) { quat_to_matrix(qB, out_m); set_translation(out_m, pB); return; }
    // !hasA && !hasB: undefined by design, caller does not use this path.

    if (dot4(qA + qB, qA + qB) < dot4(qA - qB, qA - qB)) negate4(qB);
    float q[4];
    fastproc_quat_blend(qA, qB, blend, q); // g_FastProc[22]
    quat_to_matrix(q, out_m);               // g_FastProc[14]

    float p[3];
    p[0] = (1.0f - blend) * pA[0] + blend * pB[0];
    p[1] = (1.0f - blend) * pA[1] + blend * pB[1];
    p[2] = (1.0f - blend) * pA[2] + blend * pB[2];
    out_m[3] = p[0];
    out_m[7] = p[1];
    out_m[11] = p[2];
}
```

---

## 5. Каноническая модель данных для toolchain

Ниже правила, по которым удобно строить editor/writer. Они верифицированы на corpus (435 моделей), и совпадают с тем, как устроены оригинальные ассеты.

### 5.1. Декомпозиция key pool на track-и узлов (канонично)

Для `Res1` stride 38:
- `fallback_i = node[i].hdr3`.
- `start_i = (i == 0) ? 0 : (fallback_{i-1} + 1)`.
- track узла `i` = `Res8[start_i .. fallback_i]`.

Наблюдаемые инварианты:
- `fallback_i` строго возрастает по `i`.
- track всегда непустой (`fallback_i >= start_i`).
- для узлов без map (`hdr2 == 0xFFFF`) track длиной ровно 1 ключ.
- для узлов с map track длиной минимум 2 ключа.

### 5.2. Временная ось ключей (канонично)

В observed corpus:
- `time` всех ключей — целые неотрицательные float (`0.0, 1.0, ...`).
- внутри track: строго возрастают.
- `time(start_i) == 0.0` у каждого узла.
- глобальный `Res19.attr2 == max_i(time(fallback_i)) + 1`.

### 5.3. Компоновка Res19 map-блоков (канонично)

Если `Res19.size > 0`:
- map-блоки есть только у узлов с `hdr2 != 0xFFFF`;
- длина блока каждого такого узла: `frameCount = Res19.attr2`;
- блоки идут подряд, без дыр и overlap;
- итог: `Res19.attr1 == animated_node_count * frameCount`.

Если модель статическая:
- `Res19.size == 0`, `Res19.attr1 == 0`, `Res19.attr2 == 1`, `Res19.attr3 == 2`;
- у всех узлов `hdr2 == 0xFFFF`.

### 5.4. Семантика `map_words[f]` в каноничном writer

Для кадра `f` и track `keys[start..end]`:
- если `f < keys[start].time` или `f >= keys[end].time` -> писать `fallback = end`;
- иначе писать индекс левого ключа сегмента (`start <= idx < end`) такого, что:
  - `keys[idx].time <= f < keys[idx+1].time`.

В исходных данных fallback-фреймы кодируются значением `== fallback` (не просто `>= fallback`).

---

## 6. Reference IR для редактора/конвертера

Рекомендуемое промежуточное представление:

```c
struct NodeAnimTrack {
    uint32_t node_index;
    bool     has_map;          // hdr2 != 0xFFFF
    uint16_t fallback_key;     // hdr3 (derived on write)
    vector<AnimKey> keys;      // local keys for node
    vector<uint16_t> frame_map; // optional, size == frame_count when has_map
};

struct AnimModel {
    uint32_t frame_count;      // Res19.attr2
    vector<NodeAnimTrack> tracks; // in node order
};
```

Где `AnimKey`:
- `pos: float3`,
- `time: float`,
- `quat_raw: int16[4]` (для lossless),
- `quat_decoded: float4` (опционально для API/UI).

---

## 7. Алгоритм чтения (reader)

1. Загрузить `Res1`, `Res8`, `Res19`.
2. Проверить `Res8.size % 24 == 0`, `Res19.size % 2 == 0`.
3. Для каждого узла `i` (stride 38):
   - взять `hdr2/hdr3`;
   - вычислить `start_i` через предыдущий `hdr3`;
   - извлечь `keys[start_i..hdr3]`;
   - если `hdr2 != 0xFFFF`, взять `frame_map = Res19[hdr2 : hdr2 + frame_count]`.
4. Валидировать, что map-значения либо `< hdr3`, либо fallback (`== hdr3` канонично).

---

## 8. Алгоритм записи (writer)

Нормативный минимум для runtime-совместимости:

1. Собрать keys всех узлов в один `Res8` pool в node-order.
2. Записать `hdr3 = end_index` каждого узла.
3. Вычислить `frame_count` и записать в `Res19.attr2`.
4. Для узлов с map:
   - `hdr2 = cursor`;
   - append `frame_count` слов в `Res19`;
   - `cursor += frame_count`.
5. Для узлов без map: `hdr2 = 0xFFFF`.
6. Выставить атрибуты:
   - `Res8.attr1 = key_count`, `Res8.attr2 = 0`, `Res8.attr3 = 4`;
   - `Res19.attr1 = map_word_count`, `Res19.attr3 = 2`.

Каноничный writer (рекомендуется):
- генерирует map по правилу §5.4;
- fallback-фреймы записывает `== fallback`;
- для статических узлов использует 1 ключ (`time=0`, `hdr2=0xFFFF`).

---

## 9. Валидация перед сохранением

Обязательные проверки:

1. `Res8.size % 24 == 0`, `Res19.size % 2 == 0`.
2. Для каждого узла: `fallbackKeyIndex < key_count`.
3. Если `hdr2 != 0xFFFF`: `hdr2 + frame_count <= map_word_count`.
4. Для map-сегмента узла:
   - любое значение `< fallback` должно удовлетворять `value + 1 < key_count`.
5. В track узла:
   - `time` строго возрастает;
   - при наличии map минимум 2 ключа.
6. `frame_count > 0` (игровые ассеты используют минимум 1).

Рекомендуемые проверки (каноничность):

1. `fallback_i` строго возрастает по узлам.
2. track каждого узла начинается с `time == 0`.
3. `frame_count == max_end_time + 1`.
4. map-блоки узлов без дыр/overlap.

---

## 10. Edge cases и совместимость

### 10.1. `Res19.size == 0`

Поддерживается runtime-ом:
- `frame_count` обычно 1;
- `hdr2 == 0xFFFF` у всех узлов;
- сэмплирование всегда через fallback key (`hdr3`).

### 10.2. Узлы без map

Это нормальный режим для статических/квазистатических узлов:
- `hdr2 = 0xFFFF`;
- `hdr3` указывает на единственный ключ узла (канонично).

### 10.3. `Res1.attr3 == 24` (legacy outlier)

В corpus встречается единично (`MTCHECK.MSH`, `testdata/nres/system.rlb`):
- `Res1.attr3 = 24`;
- `Res8` содержит 1 ключ;
- `Res19.size == 0`.

Алгоритм `sub_10012880` адресует node как stride 38, поэтому этот случай нельзя интерпретировать правилами текущего 38-byte формата. Практически это отдельный legacy-формат/legacy-path вне описанного runtime-контракта.

### 10.4. Квантование quaternion при экспорте

Для новых данных:
- используйте `round(q * 32767)`;
- clamp к `[-32767, 32767]` (каноничный диапазон ассетов).

---

## 11. Reference pseudocode (1:1 runtime path)

```c
void sample_node_pose(Model *m, int node_idx, float t, float out_quat[4], float out_pos[3]) {
    Node38 *node = (Node38 *)((uint8_t *)m->res1 + node_idx * 38);
    uint16_t map_start = node->hdr2;
    uint16_t fallback  = node->hdr3;
    uint32_t frame_cnt = m->anim_frame_count; // Res19.attr2

    int32_t frame = x87_fistp_i32((double)t - 0.5); // strict path

    uint16_t key_idx;
    if ((uint32_t)frame >= frame_cnt ||
        map_start == 0xFFFF ||
        m->res19[map_start + (uint32_t)frame] >= fallback) {
        key_idx = fallback;
        decode_key_quat_pos(&m->res8[key_idx], out_quat, out_pos);
        return;
    }

    key_idx = m->res19[map_start + (uint32_t)frame];
    AnimKey24 *k0 = &m->res8[key_idx];
    if (t == k0->time) {
        decode_key_quat_pos(k0, out_quat, out_pos);
        return;
    }

    AnimKey24 *k1 = &m->res8[key_idx + 1];
    if (t == k1->time) {
        decode_key_quat_pos(k1, out_quat, out_pos);
        return;
    }

    float a = (t - k0->time) / (k1->time - k0->time);
    out_pos[0] = lerp(k0->pos_x, k1->pos_x, a);
    out_pos[1] = lerp(k0->pos_y, k1->pos_y, a);
    out_pos[2] = lerp(k0->pos_z, k1->pos_z, a);
    fastproc_quat_interp(decode_quat(k0), decode_quat(k1), a, out_quat); // g_FastProc[17]
}
```

## 12. Границы полноты

Для основного формата (`Res1` stride 38 + `Res8` + `Res19`) эта страница покрывает runtime и toolchain-поведение на уровне, достаточном для 1:1 реализации (reader/writer/converter/editor).

Единственный подтверждённый неполный сегмент:
- legacy `Res1.attr3 == 24` (`MTCHECK.MSH`), для которого в `AniMesh` не найден отдельный открытый decode-path в рамках текущего реверса.

Для абсолютных 100% по всем историческим вариантам формата дополнительно нужно:
- найти и дореверсить runtime-код, который реально обрабатывает `Res1.attr3==24` (если он есть в других модулях/ветках);
- получить больше образцов `*.msh` с `attr3==24` для проверки writer/validator-инвариантов.
