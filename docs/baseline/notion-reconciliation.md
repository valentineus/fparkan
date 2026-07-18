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
