# MSH animation

Документ описывает анимационные ресурсы MSH: `Res8`, `Res19` и runtime-интерполяцию.

---

## 1.13. Ресурсы анимации: Res8 и Res19

- **Res8** — массив анимационных ключей фиксированного размера 24 байта.
- **Res19** — `uint16` mapping‑массив «frame → keyIndex` (с per-node смещением).

### 1.13.1. Формат Res8 (ключ 24 байта)

```c
struct AnimKey24 {
    float  posX;       // +0x00
    float  posY;       // +0x04
    float  posZ;       // +0x08
    float  time;       // +0x0C
    int16_t qx;        // +0x10
    int16_t qy;        // +0x12
    int16_t qz;        // +0x14
    int16_t qw;        // +0x16
};
```

Декодирование quaternion-компонент:

```c
q = s16 * (1.0f / 32767.0f)
```

### 1.13.2. Формат Res19

Res19 читается как непрерывный массив `uint16`:

```c
uint16_t map[];   // размер = size(Res19)/2
```

Per-node управление mapping'ом берётся из заголовка узла Res1:

- `node.hdr2` (`Res1 + 0x04`) = `mapStart` (`0xFFFF` => map отсутствует);
- `node.hdr3` (`Res1 + 0x06`) = `fallbackKeyIndex` и одновременно верхняя граница валидного `map`‑значения.

### 1.13.3. Выбор ключа для времени `t` (`sub_10012880`)

1) Вычислить frame‑индекс:

```c
frame = (int64)(t - 0.5f);   // x87 FISTP-путь
```

Для строгой 1:1 эмуляции используйте именно поведение x87 `FISTP` (а не «упрощённый floor»), т.к. путь в оригинале опирается на FPU rounding mode.

2) Проверка условий fallback:

- `frame >= model.animFrameCount` (`model+0x9C`, из `NResEntry(Res19).attr2`);
- `mapStart == 0xFFFF`;
- `map[mapStart + frame] >= fallbackKeyIndex`.

Если любое условие истинно:

```c
keyIndex = fallbackKeyIndex;
```

Иначе:

```c
keyIndex = map[mapStart + frame];
```

3) Сэмплирование:

- `k0 = Res8[keyIndex]`
- `k1 = Res8[keyIndex + 1]` (для интерполяции сегмента)

Пути:

- если `t == k0.time` → взять `k0`;
- если `t == k1.time` → взять `k1`;
- иначе `alpha = (t - k0.time) / (k1.time - k0.time)`, `pos = lerp(k0.pos, k1.pos, alpha)`, rotation смешивается через fastproc‑интерполятор quaternion.

### 1.13.4. Межкадровое смешивание (`sub_10012560`)

Функция смешивает два сэмпла (например, из двух animation time-позиций) с коэффициентом `blend`:

1) получить два `(quat, pos)` через `sub_10012880`;
2) выполнить shortest‑path коррекцию знака quaternion:

```c
if (|q0 + q1|^2 < |q0 - q1|^2) q1 = -q1;
```

3) смешать quaternion (fastproc) и построить orientation‑матрицу;
4) translation писать отдельно как `lerp(pos0, pos1, blend)` в ячейки `m[3], m[7], m[11]`.

### 1.13.5. Что хранится в `Res19.attr2`

При загрузке `sub_10015FD0` записывает `NResEntry(Res19).attr2` в `model+0x9C`.
Это поле используется как верхняя граница frame‑индекса в п.1.13.3.

---

