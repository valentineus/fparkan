# II. Запуск, архитектура и игровой цикл

Этот том описывает путь от запуска `iron_3d.exe` до устойчивого кадра:
загрузку `iron3d.dll`, создание shell/game objects, поднятие платформенных
сервисов, запуск World3D, расчёт simulation step, безопасное удаление объектов,
рендер и завершение программы.

Главная особенность Iron3D -- это не один монолитный engine object, а связка
небольшого Win32 bootstrap и набора DLL, которые обмениваются фабриками,
singleton-интерфейсами и C++ vtable. Совместимая реализация может изменить
физическое деление на библиотеки, но не может произвольно менять порядок
инициализации, object identity, правила владения, fallback ресурсов и порядок
событий.

```text
iron_3d.exe
  -> iron3d.dll
     -> services.dll
     -> World3D.dll
     -> Terrain.dll
     -> Ngi32.dll
     -> AniMesh.dll / ArealMap.dll / Effect.dll
     -> ai.dll / Behavior.dll / Wizard.dll
     -> Control.dll / MisLoad.dll / Net.dll / Joystick.dll
```

## Карта модулей

Во внешней архитектуре обнаружено пятнадцать DLL. Экспортов сравнительно мало:
они обычно создают объект, возвращают singleton или дают доступ к уже поднятой
подсистеме. Основная работа выполняется через C++-интерфейсы, поэтому порядок
виртуальных слотов является частью ABI, особенно для compatibility shim эпохи
MSVC6.

```text
iron_3d.exe
    |
    v
iron3d.dll  -- композиция игры, shell и главный цикл
    |
    +-- services.dll -- доступ к display, GUI, ресурсам, звуку, таймеру и сети
    +-- World3D.dll  -- объекты, очередь, время, камера и кадр
    +-- Terrain.dll  -- ландшафт, свет, атмосфера и визуальный слой мира
    +-- ai.dll / Behavior.dll / Wizard.dll
    +-- Control.dll / Effect.dll / MisLoad.dll
    +-- Net.dll / Joystick.dll
    +-- Ngi32.dll    -- ресурсы, графика, звук, математика и CPU dispatch
```

Циклы импортов между DLL ожидаемы. Terrain создаёт визуальные объекты и
обращается к World3D, а World3D получает world-interface из Terrain. Это не
значит, что обе библиотеки совместно владеют всем состоянием. Реальные границы
задаются интерфейсами, refcount, очередью объектов и порядком shutdown.

Практичная новая структура может быть внутренним набором модулей `platform`,
`resources`, `world`, `mission`, `terrain`, `render`, `animation`, `effects`,
`behavior`, `physics`, `audio` и `network`. Важно сохранить не DLL-границы, а
контракты: имена ресурсов, порядок поиска, fallback-ветки, object ID, момент
создания mirror objects, численное поведение и последовательность событий.

## Роли модулей

`iron3d.dll` создаёт shell и game objects, читает `iron_3d.ini`, поднимает
display, sound, CD-audio, network и настройки World3D, загружает миссионные и
UI-конфигурации, содержит message pump и вызывает расчёт/рендер игры.

`services.dll` работает как service locator. Через него запрашиваются display,
GUI, network manager, resource manager, sound server и timer. Этот слой отделяет
высокоуровневую игру от деталей создания устройств.

`World3D.dll` -- центральный runtime: очередь объектов, идентификаторы,
события, отложенное удаление, game time, pause, manual input, камера,
material/texture/lightmap managers, сетевые mirrors, расчёт и 3D-проход.

`Terrain.dll` отвечает не только за землю. В его область входят ландшафт,
здания, визуальный слой мира, камера, shade/state layer, primitive buffers,
сортировочные слои, источники света, тени, microtextures, атмосфера, дождь,
молнии, солнце и flares.

`Ngi32.dll` содержит низкоуровневые сервисы: DirectDraw/Direct3D-era renderer,
DirectSound, readers `NRes`/`RsLi`, память, часы, математику, пересечения,
определение CPU и таблицу быстрых процедур `g_FastProc`.

Предметные DLL закрывают отдельные области. `AniMesh.dll` загружает модели и
агентов. `ArealMap.dll` строит spatial graph и маршруты. `Behavior.dll`
реализует поведение юнитов. `ai.dll` содержит стратегический AI и миссионные
сценарии. `Wizard.dll` корректирует локальное движение. `Control.dll`
обслуживает физическую модель и столкновения. `Effect.dll` создаёт runtime-FX.
`MisLoad.dll` читает миссионные данные. `Net.dll` инкапсулирует DirectPlay.
`Joystick.dll` работает через DirectInput.

## Поток данных

Миссия не создаёт готовый кадр напрямую. Данные проходят через несколько
уровней: описание объекта, прототипы, ресурсы, runtime-object, контроллеры,
simulation state, render items и только затем платформенный renderer.

```text
mission data
  -> object identity and properties
  -> prototype registry
  -> model/material/texture/effect resources
  -> World3D object + domain controllers
  -> simulation state
  -> visible render items
  -> Ngi32 render interface
  -> DirectX-era device
```

Этот поток объясняет, почему нельзя объединять физический архив, metadata entry,
декодированный payload и готовый runtime-кэш. У каждого уровня свой срок жизни,
собственный refcount и собственные ошибки. Детали ресурсного конвейера описаны
в [Томе III](03-resources.md), а сборка мира из миссии -- в [Томе IV](04-world.md).

## Bootstrap

`iron_3d.exe` -- небольшой PE32/x86 bootstrap размером 36 864 байта. Основная
игровая логика находится в `iron3d.dll`. Исполняемый файл создаёт Win32-процесс,
подготавливает окружение, загружает библиотеку и получает восемь публичных
точек входа:

```text
createShell       deleteShell
createGame        deleteGame
createSubsystems  deleteSubsystems
getIGame          getIShell
```

Эти функции образуют внешнюю границу игры. `createShell` создаёт оболочку
интерфейса и меню, `createGame` -- объект игровой логики, `createSubsystems` --
аппаратные и runtime-сервисы. Getter-функции возвращают уже созданные объекты.

Запуск удобно читать как конечный автомат:

```text
PROCESS_CREATED
  -> LIBRARY_READY
  -> ENTRYPOINTS_READY
  -> SHELL_CREATED
  -> GAME_CREATED
  -> SUBSYSTEMS_READY
  -> MAIN_LOOP
  -> SUBSYSTEMS_CLOSED
  -> GAME_DELETED
  -> SHELL_DELETED
```

Каждый переход имеет обратное действие. Если display, sound или другой
обязательный сервис не создан, главный цикл не начинается, но уже созданные
объекты освобождаются в обратном порядке. Новая оболочка запуска должна
работать из каталога оригинальной установки, сохранять смысл относительных
путей, создавать окно до графической подсистемы и закрывать частично поднятые
сервисы без предположения, что init дошёл до конца.

Bootstrap обеих полных частей побайтно одинаков, хотя файл второй части может
иметь другое имя:

```text
size       36 864
entry RVA  0x147E
SHA-256    f476af85c034a4b4f34f49d0806e4dff397b5da0ee26d382a7674231144979f7
```

Следовательно, различия полных частей начинаются после передачи управления DLL
и игровым данным. Адреса executable демоверсии относятся к другой binary
profile и не должны переноситься на полные версии без проверки hash.

## Инициализация подсистем

Iron3D разделяет создание высокоуровневых объектов и создание подсистем.
`createShell` конструирует оболочку пользовательского интерфейса, `createGame`
создаёт объект игры, а `createSubsystems` связывает их с display, sound,
network и World3D.

Высокоуровневая последовательность выглядит так:

```text
прочитать iron_3d.ini
  -> получить display service
  -> создать окно и графическое устройство
  -> проверить доступность 3D-драйвера
  -> выбрать CURRENT_D3DCARD
  -> получить sound service и настроить громкость
  -> создать network instance и передать application GUID
  -> создать World3D game settings
```

Ошибка отсутствующего 3D-устройства обрабатывается отдельно от ошибок ресурсов:
это разные стадии запуска. Конфигурация влияет не только на разрешение. В
runtime попадают графическая карта, громкость эффектов, CD-audio, режим
CD-sound, сетевое приложение и World3D settings. Application GUID сетевой
подсистемы:

```text
{3C1D1F01-A870-11D1-8400-000021B14415}
```

Один и тот же GUID передаётся сетевому объекту и service layer. Если он
разойдётся, экземпляры игры станут логически разными приложениями, даже при
исправном транспорте.

## `stdInitGame`

После платформенных сервисов World3D создаёт внутренний runtime:

1. Создаёт глобальную очередь объектов.
2. Сохраняет window handle и режим игры.
3. При нужном режиме ограничивает курсор областью окна.
4. Получает или создаёт 3D sound object.
5. Загружает реестр адресов компонентов из `Comp.ini`.
6. Получает или создаёт 3D renderer.
7. Читает профиль возможностей renderer.
8. Загружает component type 6.
9. Для multiplayer создаёт NetWatcher.
10. Получает world-interface из Terrain.
11. Устанавливает исходные параметры света и тумана.

Порядок важен. World objects не должны появляться до queue, ресурсы рендера --
до renderer, сетевые mirror objects -- до NetWatcher. В новой реализации у
каждого этапа должен быть явный признак успешного создания, чтобы shutdown мог
безопасно разобрать неполный init.

## Завершение

Shutdown идёт в обратном направлении: прекращаются игровые расчёты и сетевые
наблюдатели, разбираются отложенные операции, освобождаются world objects и
менеджеры, затем renderer и sound, затем game settings и platform services.
Ограничение курсора снимается, глобальные ссылки очищаются.

Полезный протокол завершения:

1. Запретить новые события и новые объекты.
2. Дождаться выхода из calculation/render traversal.
3. Разобрать очередь deferred operations.
4. Отсоединить объекты от очереди, контроллеров и менеджеров.
5. Освободить managers и singletons после их consumers.
6. Закрыть устройства и платформенные сервисы.

Такой порядок защищает от dangling-ссылок между World3D, Terrain, renderer,
sound и сетевым слоем.

## Главный цикл

Главный цикл -- не одна функция `update_and_render`, а расписание, связывающее
Win32 messages, input, игровые события, таймеры, сеть и renderer. Системная
очередь сообщает об активации окна, вводе, изменении состояния процесса и
выходе. Очередь World3D рассчитывает игровые объекты. У этих очередей разные
правила времени и владения, поэтому их нельзя смешивать в один контейнер.

Подтверждённые точки вызова в одном из профилей:

```text
stdCalculateGame       RVA 0x5FA94, 0x604C1, 0x6086B
ClearManualEventsList  RVA 0x6052F
stdRenderGame          RVA 0x60B2F
UpdateManualEventsList в обработчике сообщений около RVA 0xA3759
```

Смысловой skeleton:

```c
while (running) {
    stdCalculateGame();
    clear_keyboard_snapshot();
    update_shell_and_mode();
    ClearManualEventsList();
    process_window_messages();
    update_timers_ui_gameplay_network();
    if (mode_requires_extra_step) stdCalculateGame();
    if (render_enabled) {
        stdSetCurrentCamera(camera);
        stdRenderGame(camera);
    } else {
        sleep_briefly();
    }
    update_post_render_state();
}
```

Ввод из window messages накапливается между расчётными шагами. Если читать
клавиатуру только внутри рендера, события будут теряться при пропущенных кадрах
или отключённом выводе.

## `stdCalculateGame`

Calculation pass сначала очищает или подготавливает список manual events,
увеличивает внутренний depth/counter и опрашивает input device. Если устройство
временно потеряно, выполняется повторное получение доступа и чтение повторяется.
Затем при незамороженной игре выставляется признак `in_calculation` и вызывается
основной traversal очереди объектов.

```text
prepare input/events
  -> enter calculation
  -> dispatch queue events
  -> objects update behavior and transforms
  -> leave calculation
  -> apply deferred operations
  -> occasional cache maintenance
```

После traversal разбирается deferred-delete list. Объект может запросить
собственное удаление во время события, но память освобождается только после
завершения обхода. Периодически также очищаются давно неиспользуемые ресурсы и
объекты по порогам часов порядка 20 и 60 секунд.

Совместимый runtime должен иметь явный traversal depth или флаг
`in_calculation`. Нельзя полагаться на то, что контейнер выдержит удаление
текущего элемента из обработчика события.

## Жизненный цикл кадра

Рендер читает состояние, подготовленное расчётом. Кадр начинается до renderer-а:
message pump уже накопил ввод, World3D уже обновил объекты, отложенные операции
и анимации, после чего выбирается камера и обновляется listener звука.

```text
system messages and input
  -> simulation calculation
  -> deferred object operations
  -> animation and transforms
  -> camera and sound listener
  -> visibility and render queues
  -> materials and draw passes
  -> renderer completion
  -> end-of-render callbacks and UI
```

В `World3D::stdRenderGame` виден крупный каркас: установка camera/viewport,
renderer frame boundaries, traversal мира, завершение world/shade path,
renderer completion, снятие `in_render`, восстановление viewport и рассылка
end-of-render callbacks. Эти callbacks позволяют объектам безопасно обновить
временные ресурсы после того, как draw-команды больше их не используют.

Один calculation step не обязан соответствовать одному изображению. Главный
цикл допускает дополнительный вызов `stdCalculateGame` и режим, в котором
расчёт продолжается без вывода кадра. Поэтому нужно хранить отдельно:

1. монотонные платформенные часы;
2. игровое время с pause и масштабированием;
3. длительность текущего calculation step;
4. локальное время анимации и FX;
5. реальные часы обслуживания кэшей.

Игровую логику нельзя выводить из render delta: изменение частоты кадров тогда
изменит движение, камеру и сценарные таймеры. Подробности render item и рисков
кадровой совместимости вынесены в справочник [Render frame](../reference/render-frame.md).

## World3D

World3D связывает игровые объекты, события, время, ввод, камеру, сетевые
отражения и визуальное представление. Он не содержит всю предметную логику:
движение делегируется Behavior/Wizard, физика -- Control, мир -- Terrain. Его
задача -- общая идентичность, порядок вызовов и безопасный жизненный цикл.

`CreateQueue` создаёт singleton-объект размером 20 байт, а `GetQueue`
возвращает его. Очередь служит центральным маршрутизатором событий и операций
над объектами.

Публичный слой предоставляет отдельные функции для локальных и сетевых
объектов:

```text
CreateObject
AddObjectToGame
AddNewObjectToGame
CreateMirrorObject
AddMirrorObjectToGame
AddNewMirrorToGame
```

Разделение "создать" и "добавить в игру" означает два этапа: сначала выделить и
настроить instance, затем зарегистрировать его в общей системе. Это позволяет
loader-у заполнить свойства до появления объекта в расчётной очереди.

## Идентичность объектов

Object ID кодирует не только порядковый номер. Проверки диапазонов показывают
разбиение на номер игрока, класс и индекс. Mirror object представляет объект,
владельцем которого является другой участник. Локальный runtime хранит его
видимое состояние, но источник авторитетных изменений находится удалённо.

```c
struct ObjectId {
    uint32_t raw;
    uint16_t owner_player;
    uint16_t class_and_index;
};
```

Точное битовое разбиение нужно брать из сетевых функций. На уровне API уже
сейчас полезно разделить логические свойства: `is_local`, `is_mirror`, `owner`,
`class` и `index`.

Минимальный runtime-object должен хранить identifier, type, owner, transform,
active state, ordered property bag, ссылки на controllers, участие в расчёте и
рендере, сетевой статус и флаг отложенного удаления. Специализированные DLL
могут быть представлены компонентами, но порядок их вызовов задаёт World3D.

## Отложенное удаление

`DeleteGameObject` проверяет, идёт ли calculation pass. Если обход активен и
удаление не принудительное, объект помещается в deferred list. `KillGameObject`
отправляет запрос через очередь, а не освобождает память напрямую.

```c
void request_delete(Object* o) {
    if (world.in_calculation) {
        world.deferred_delete.push_back(o);
        o->pending_delete = true;
    } else {
        world.detach_and_release(o);
    }
}
```

Это защищает итераторы, связи и текущий стек вызовов. Любая новая подсистема,
способная удалить объект из обработчика события, обязана пользоваться тем же
механизмом.

Регистрация в очереди и владение памятью -- разные понятия. Удаление из мира не
всегда означает немедленное освобождение instance: часть объектов и managers
использует intrusive reference count, а renderer, sound и resource managers
могут возвращать уже существующий singleton с увеличенным счётчиком. Поэтому
global manager закрывается после всех объектов, которые на него ссылаются.

## Детерминизм

Даже при одинаковых формулах результат зависит от порядка. Стабильный runtime
сохраняет последовательность queue traversal, момент формирования input
snapshot, порядок сетевых сообщений, обработку deferred operations и порядок
обращений к RNG. Оптимизация и многопоточность допустимы только при
детерминированном объединении результатов.

Для переносимой реализации полезно разделить scheduler phases и immutable
render snapshot. Это архитектурная рекомендация для новой реализации, а не
утверждение о точном layout исходных C++ classes.

## Стабильность между сборками

Внешняя архитектура полных Частей 1 и 2 сохраняет те же пятнадцать DLL, 313
exports, имена, ordinals и import sets. Побайтно идентичны:

```text
ai.dll, Behavior.dll, Joystick.dll, MisLoad.dll, Net.dll,
Ngi32.dll, Terrain.dll, Wizard.dll, World3D.dll
```

Пересобраны:

```text
AniMesh.dll, ArealMap.dll, Control.dll, Effect.dll,
iron3d.dll, services.dll
```

Это разделяет переносимость выводов. World3D lifecycle, Terrain, NRes/RsLi
readers, mission loader, AI/Behavior/Wizard, DirectPlay wrapper и joystick
adapter подтверждаются одной машинной реализацией. Model/agent runtime,
collision, effects, shell/composition и service layer требуют отдельного
сравнения поведения Частей 1 и 2.

Для `World3D.dll` Частей 1 и 2 применим общий hash:

```text
World3D.dll SHA-256
17e4a3089b2583a8cf2356c9db0390b1aba138356a09130d79b4e7e4791da61e
```

RVA внутри `iron3d.dll` нельзя считать общими без проверки конкретного файла:
эта DLL пересобрана между частями, а демоверсия имеет отдельный binary profile.
Смысловая последовательность цикла переносится как контракт scheduler-а, но
адреса остаются build-specific.
