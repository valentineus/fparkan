# Глоссарий

Глоссарий объясняет термины в том смысле, в котором они используются в этой
книге. Короткое определение не заменяет профильную главу: практический контракт
понятия раскрывается в соответствующем томе или справочной странице.

## Бинарные файлы и ABI

**PE (Portable Executable)** -- формат исполняемых файлов Windows: EXE и DLL.
Он содержит заголовки, секции, таблицы импортов и экспортов, relocations и
адрес точки входа.

**Image base** -- предпочтительный адрес начала загруженного PE-образа.
**VA** -- виртуальный адрес в процессе. **RVA** -- адрес относительно image
base.

**Import** -- внешняя функция или переменная, которую модуль получает из другой
DLL. **Export** -- символ, предоставляемый другим модулям. Имя, ordinal и
calling convention вместе образуют часть binary contract.

**ABI** -- соглашение о двоичном взаимодействии: размещение аргументов, возврат
значений, очистка stack, layout структур, порядок virtual methods и правила
владения.

**Calling convention** -- часть ABI, определяющая передачу аргументов и очистку
stack. Для исследованного 32-bit code важны `__cdecl`, `__stdcall` и
`__thiscall`.

**Vtable** -- массив указателей на virtual methods C++-объекта. Запись
`vtable +0x34` означает вызов указателя по байтовому смещению `0x34` от начала
таблицы.

**Static analysis** исследует файл без исполнения: disassembly, strings,
imports, call graph и data flow. **Dynamic analysis** наблюдает работающую
программу: breakpoints, traces, API hooks, memory state и packet/frame captures.

**Evidence** -- повторяемое наблюдение. **Inference** -- вывод, объединяющий
несколько наблюдений. **Hypothesis** -- рабочее предположение, ещё не
подтверждённое достаточным экспериментом.

## Форматы данных

**Archive** -- контейнер, объединяющий множество ресурсов. **Entry** -- запись
его каталога. **Payload** -- полезные bytes конкретной записи.

**Magic** -- короткая сигнатура формата, например `NRes` или `Texm`.
**Version** -- номер варианта layout. Проверка одной magic без проверки version
и размеров недостаточна.

**Offset** -- положение данных относительно начала файла или структуры.
**Size** -- число bytes. **Stride** -- размер одного элемента массива.
**Alignment** -- требование начинать данные на offset, кратном заданному числу.

**Little-endian** -- порядок, в котором младший byte многобайтного числа
расположен первым. Основные числовые поля форматов Iron3D используют этот
порядок.

**Fixed-size string** -- поле заранее известной длины. Полезная строка
заканчивается первым NUL, но оставшиеся bytes могут содержать служебный хвост и
должны сохраняться.

**Opaque field** -- поле с доказанными offset и size, но не установленным
предметным смыслом. Его безопасно читать и копировать, но нельзя очищать или
переосмысливать без эксперимента.

**Invariant** -- условие, которое обязано выполняться: range лежит внутри
payload, индекс указывает на существующий элемент, count соответствует размеру
секции.

**Strict reader** отклоняет любое нарушение контракта. **Compatibility reader**
дополнительно воспроизводит только известные особенности оригинала.

**Fallback** -- явно предписанный запасной путь, например material `DEFAULT`,
затем entry 0. **Heuristic** -- догадка по похожим данным; она не должна
незаметно заменять доказанный fallback.

**Roundtrip** -- последовательность decode -> encode. **Byte-identical
roundtrip** создаёт файл, полностью совпадающий с исходным. **Lossless editor**
может изменить известное поле, сохранив все остальные bytes и порядок записей.

## Ресурсы

**NRes** -- основной контейнер ресурсов с каталогом в конце файла.

**RsLi** -- библиотечный архив с каталогом в начале файла и несколькими методами
упаковки payload.

**TMA** -- mission data: paths, clans, placed objects, properties, land path и
extras.

**MSH** -- модель Iron3D, представленная как NRes с entries для geometry,
nodes, slots, batches, animation и auxiliary streams.

**WEAR** -- таблица внешнего вида модели, переводящая material index в MAT0
name и lightmap slots.

**MAT0** -- материал: phases, parameters, animation blocks и texture references.

**Texm** -- texture payload с header, palette, mip chain и optional Page atlas.

**FXID** -- ресурс эффектов: команды, references, lifetime, random/time modes и
runtime instances.

## Игровой runtime

**Engine** -- программная среда, которая загружает данные, ведёт время,
исполняет мир и формирует изображение/звук. **Game** -- правила, миссии и
content поверх engine services.

**World** -- долгоживущее состояние миссии: objects, terrain, время, кланы и
managers. **Scene** -- представление части мира для конкретной обработки,
обычно текущей камеры.

**Game object** -- сущность с идентичностью, transform, properties и lifecycle.
**Component/controller** -- специализированная часть поведения: animation,
physics, AI или rendering representation.

**Simulation** отвечает за изменение мира. **Tick** -- один расчётный шаг.
**Frame** -- одно подготовленное изображение. Число ticks и frames за единицу
времени не обязано совпадать.

**Event/message** -- типизированное сообщение между objects или subsystems.
**Queue traversal** -- стабильный обход зарегистрированных объектов.
**Deferred deletion** -- перенос фактического удаления до безопасной границы.

**Snapshot** -- согласованное состояние, которое renderer читает без изменения
simulation. **Determinism** -- одинаковый результат при одинаковом initial
state, input, времени и порядке событий.

**Authority** -- subsystem или network peer, которому разрешено окончательно
менять состояние объекта. **Mirror object** -- локальное представление объекта,
authority которого находится у другого player.

## Геометрия и рендеринг

**Vertex** -- вершина geometry. **Index** -- номер вершины. **Triangle** --
примитив из трёх индексов.

**Node** -- элемент hierarchy модели со своим local transform. **Slot** в MSH
-- выбранная геометрическая группа для комбинации node, LOD и group. **Batch**
-- непрерывный индексный диапазон с material slot и render state.

**Transform** переводит данные между coordinate spaces. **Matrix** задаёт
линейное преобразование и translation. Порядок умножения matrices является
частью контракта.

**Bounds** -- упрощённый объём для быстрых тестов. **AABB** -- min/max по осям.
**Bounding sphere** -- center и radius.

**Renderer** преобразует подготовленную сцену в изображение. **Backend** --
реализация поверх конкретного API или устройства.

**Draw call** -- команда нарисовать диапазон primitives. **Indexed draw**
использует index buffer и base vertex.

**Material phase** -- одно временное состояние анимированного материала.
**Texture** -- двумерный массив texels. **Mip chain** -- последовательность
уменьшенных уровней texture. **Atlas** -- texture с несколькими под-
изображениями.

**Fixed-function pipeline** -- старый graphics pipeline, где приложение
выбирает predefined transform, lighting, texture-stage и blend states вместо
пользовательских shaders.

**Depth test**, **culling**, **alpha test** и **blending** -- render states,
которые влияют на порядок и видимость fragments.

**Pixel parity** -- совпадение конечного изображения при фиксированных camera,
time, seed, resolution и device profile.

## Навигация, звук и сеть

**Areal** -- логическая область карты с границей, class/flags и связями с
соседями. **Areal graph** -- граф областей и переходов. **Cell grid** --
пространственный индекс для быстрых candidate queries.

**Pathfinding** -- поиск маршрута по graph. **Corridor** -- локальная полоса,
построенная из последовательности areals. **Local steering** корректирует
ближайший шаг внутри corridor.

**Collision proxy** -- упрощённое представление объекта для столкновений.
**Broad phase** быстро находит потенциальные пары. **Narrow phase** выполняет
точную проверку и вычисляет contact.

**Sample** -- декодированные звуковые данные. **Source** -- конкретный
экземпляр воспроизведения с position, gain, loop state и временем. **Listener**
-- положение и ориентация слушателя для 3D spatialization.

**Transport** -- механизм доставки bytes между peers. **Protocol** -- framing,
message types, порядок и правила подтверждения. **Wire compatibility** --
способность обмениваться данными с оригинальным клиентом.

**Serialization** -- преобразование typed state в byte sequence. **Framing** --
способ отделить одно сообщение от следующего. **Reliable delivery** гарантирует
доставку/порядок в пределах выбранной модели; **unreliable delivery** допускает
потери ради задержки.

**Player ID** транспорта и **game player number** -- разные идентичности.
**Ownership transfer** меняет authority объекта. **Replication** передаёт
состояние или события remote mirrors.
