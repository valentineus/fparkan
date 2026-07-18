# Сверка локальной книги с FParkan в Notion

Проверка выполнена 18 июля 2026 года по странице `FParkan`, её восьми томам,
плану Vulkan revision и приложениям A--D. Цель — не копировать структуру
Notion, а убедиться, что каждый доказательный контракт доступен в `docs/` и
остается применимым к Windows/Vulkan scope.

| Notion | Локальное место |
| --- | --- |
| Статьи 1--3 | `tomes/01-guide.md` |
| Статьи 4--8 | `tomes/02-architecture.md` |
| Статьи 9--13 | `tomes/03-resources.md` и `reference/` |
| Статьи 14--18 | `tomes/04-world.md` и `reference/tma.md` |
| Статьи 19--27 | `tomes/05-render.md`, `reference/` и `rendering/` |
| Статьи 28--32 | `tomes/06-behavior.md` |
| Статьи 33--37 | `tomes/07-implementation.md` и `baseline/vulkan-revision-plan.md` |
| Статьи 38--42 | `tomes/08-evidence.md`, `appendices/glossary.md`, `evidence/` |
| Приложение A | этот audit, `baseline/current-project-audit.md` и тематические тома |
| Приложение B | `appendices/ui-shell.md` |
| Приложение C | `appendices/saves-campaign.md` |
| Приложение D | `appendices/script-vm.md` |

В ходе сверки добавлены отсутствовавшие локальные контракты UI/Shell,
сохранений/campaign и Script VM. Повторы не переносились: форматы, ABI и
corpus statistics уже находятся рядом с соответствующими readers/consumers.

Не перенесены только противоречащие утвержденному scope цели: Linux, macOS,
MoltenVK, GLES/RG40XX и hosted CI. Они не считаются пробелами. Текущие
доказательства Windows/Vulkan и все последующие уточнения ведутся только в
локальных файлах.

## Повторная содержательная сверка (18 июля 2026)

Повторная сверка проверяет не количество дочерних страниц в Notion, а
утверждения, которые они добавляют к реализации. Источником были корневая
страница `FParkan`, восемь оглавлений с 42 статьями, а также две специальные
страницы: `План реализации stage 0–5: Vulkan revision` (редакция 18 июля) и
`Ревью перехода на Vulkan: решение, доказательства и ограничения`.

| Контракт из Notion | Локальное подтверждение | Результат |
| --- | --- | --- |
| Статьи 1–3: терминология, уровень доказательств, методика | `tomes/01-guide.md` | Полностью покрыто. |
| Статьи 4–8: bootstrap, DLL, frame loop, World3D | `tomes/02-architecture.md` | Полностью покрыто. |
| Статьи 9–13: VFS, NRes, RsLi, registry, unit и auxiliary formats | `tomes/03-resources.md` и `reference/` | Полностью покрыто. |
| Статьи 14–18: TMA, mission loader, Land, ArealMap и world construction | `tomes/04-world.md`, `reference/tma.md`, `reference/msh.md` | Полностью покрыто. |
| Статьи 19–27: Ngi32, MSH, animation, MAT0/WEAR/Texm, terrain и кадр | `tomes/05-render.md`, `reference/`, `rendering/` | Покрыто; локально дополнено более свежими evidence по D3D7 camera, Node38 и Terrain/GetShade. |
| Статьи 28–32: AI, control, camera, audio, network | `tomes/06-behavior.md`, `appendices/script-vm.md` | Полностью покрыто. |
| Статьи 33–37 и Vulkan revision: ports, stages, deterministic gates, Vulkan profile | `tomes/07-implementation.md`, `baseline/vulkan-revision-plan.md` | Полностью покрыто в Windows-only редакции. |
| Статьи 38–42 и приложения A–D: ABI, corpus, knowledge boundaries, glossary, shell, saves, VM | `tomes/08-evidence.md`, `appendices/`, `evidence/`, этот audit | Полностью покрыто. |

Пропущенных применимых технических контрактов в этом срезе не найдено. В
частности, локальный Vulkan plan уже содержит независимость решений Vulkan и
`winit`, Vulkan 1.1 baseline, `ash`-изоляцию, SPIR-V manifest/hash, capability
gates, canonical RGBA8 upload и первичность backend-neutral command capture.
Не переносились только исторические cross-platform acceptance требования из
Notion: они прямо отменены текущим Windows-only scope, а не потеряны при
синхронизации.

Следовательно, новые факты следует добавлять непосредственно в тематический
локальный документ; повторный перенос дерева или дублирование страниц Notion
не требуется.

## Проверяемые источники и правило разрешения расхождений

Содержательная сверка опирается не только на оглавления. Были прочитаны
корневая страница `FParkan`, оглавления томов I--VIII и актуальные специальные
страницы [Vulkan revision](https://app.notion.com/p/387e79f2db3981778f94cdf34db5f93f),
[Vulkan review](https://app.notion.com/p/388e79f2db39810eb649edbe90bca529),
а также исторические статьи 33--34. Это позволяет отличить контракт от
исторического статуса работы.

- В локальной книге сохранены применимые контракты Vulkan revision: Windows
  как единственная acceptance-платформа, Vulkan 1.1 baseline, изоляция
  `ash`/raw handles в adapter-е, capability gates, offline SPIR-V и первичность
  backend-neutral command capture.
- Не переносится прежний статус-аудит Notion (например, утверждения о
  synthetic-only renderer или незакрытом Windows smoke): он описывал состояние
  до последующих локальных captures и потому не является спецификацией.
- Не переносятся Linux, macOS/MoltenVK и portability-enumeration требования.
  Это сознательно исключённая область, а не пробел документации.
- Если страница Notion и свежий локальный evidence расходятся, локальный
  evidence с командой воспроизведения, артефактом и датой имеет приоритет;
  спорный факт отмечается как граница знания, пока не будет перепроверен.

Таким образом, на момент сверки не обнаружено пропущенных применимых
технических контрактов: содержательные добавления из Notion уже разнесены по
тематическим локальным документам, а новые результаты разработки должны
добавляться только локально.
