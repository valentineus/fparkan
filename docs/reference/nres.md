# NRes

`NRes` -- основной контейнер ресурсов Iron3D. Он используется как внешний
архив и как внутренний контейнер модели `*.msh`.

```text
[Header: 16 bytes]
[Data region: payload with alignment]
[Directory: entry_count * 64 bytes]
```

## Header

```c
struct NResHeader16 {
    char     magic[4];      // "NRes"
    uint32_t version;       // 0x00000100
    int32_t  entry_count;   // >= 0
    uint32_t total_size;    // equals file size
};
```

`directory_offset = total_size - entry_count * 64`. Reader проверяет отсутствие
переполнений, `directory_offset >= 16` и точное окончание каталога на
`total_size`.

## Entry

```c
#pragma pack(push, 1)
struct NResEntry64 {
    uint32_t type_id;
    uint32_t attr1;
    uint32_t attr2;
    uint32_t size;
    uint32_t attr3;
    char     name[36];
    uint32_t data_offset;
    uint32_t sort_index;
};
#pragma pack(pop)
```

Имя содержит bounded C-string до 35 полезных bytes. `sort_index` задаёт
отображение из sorted position в original entry index. В строгом режиме все
`sort_index` образуют перестановку `0..N-1`.

## Data region

Payload каждой записи лежит после header и до начала каталога. Игровые архивы
выравнивают следующий payload до 8 bytes нулями, но reader не должен требовать
плотного покрытия data region.

Различаются:

- active payload -- диапазон, на который указывает entry;
- gap/padding -- bytes между активными диапазонами;
- unindexed preserved region -- произвольные bytes, не принадлежащие entry.

Lossless editor сохраняет все три категории. Compact writer может исключить
unindexed regions только при явной операции repack.
