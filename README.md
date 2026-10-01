# openGMOD

**openGMOD** — реимплементация движка Garry's Mod с нуля на Rust, по примеру
[OpenMW](https://openmw.org) и [openOMSI](https://github.com/turbo-devv/openOMSI). Цель — 64-битный
многопоточный движок с современным рендером (wgpu: DirectX 12, запасной — Vulkan), на котором
работают контент, Lua-аддоны, геймоды и воркшоп Garry's Mod без изменений. Сейчас — под Windows;
Linux и macOS — позже.

> [!IMPORTANT]
> **Нужна лицензионная Garry's Mod.** В openGMOD нет игрового контента: он работает на файлах
> установленной игры и на аддонах воркшопа, на которые подписан её владелец. **Папку игры
> openGMOD только читает** — ничего в ней не создаёт и не меняет.

> [!WARNING]
> **Ранняя стадия.** Готово чтение контента, и карту уже можно открыть и облететь (`opengmod`).
> Самой игры пока нет: ни Lua, ни физики, ни мультиплеера. Что дальше — в
> [дорожной карте](docs/ROADMAP.md).

## Что уже есть

| Crate | Что делает |
|---|---|
| [`gmod-fs`](crates/gmod-fs) | находит GMod (указанная папка, `GMOD_ROOT`, библиотеки Steam), читает VPK и GMA и собирает файловую систему так же, как GMod: `gameinfo.txt`, подписки воркшопа (включая старые сжатые `.bin`), аддоны из `garrysmod/addons`, игры из `cfg/mount.cfg` |
| [`gmod-kv`](crates/gmod-kv) | KeyValues: материалы `.vmt`, скрипты, VGUI `.res`, локализация, файлы Steam |
| [`gmod-glua`](crates/gmod-glua) | диалект Lua из GMod (`!=`, `!`, `&&`, `\|\|`, `//`, `/* */`, `continue`) → обычный LuaJIT, строка в строку |
| [`gmod-bsp`](crates/gmod-bsp) | карты BSP v19–21: все нужные для отрисовки лампы, дисплейсменты, статические пропы, pakfile; сетка мира с атласом световых карт |
| [`gmod-material`](crates/gmod-material) | текстуры VTF 7.0–7.5 (все 30 форматов пикселей, DXT, кубмапы, анимация) и материалы VMT (параметры шейдеров, условия, патч-материалы) |
| [`gmod-model`](crates/gmod-model) | модели MDL + VVD + VTX: кости, скины, бодигруппы, сетки для отрисовки; запечённый свет статических пропов (VHV) |
| [`gmod-world`](crates/gmod-world) | карта, готовая к отрисовке: мир, брашевые энтити и статические пропы по материалам, их свет, текстуры, небо; раскладка для видеокарты |
| [`gmod-render`](crates/gmod-render) | рендер на wgpu: DirectX 12, запасной — Vulkan; текстуры DXT уходят на видеокарту без распаковки |
| [`gmod-app`](crates/gmod-app) | программа `opengmod`: окно, карта, полёт камерой |
| [`gmod-check`](tools/gmod-check) | проверка вашей установки: что openGMOD уже умеет прочитать; картинка карты без видеокарты |

Проверено на настоящих данных. Все 596 Lua-файлов GMod после перевода компилируются в LuaJIT
2.0 и 2.1. Тестовые VPK, собранные инструментами Valve, читаются с совпадающими CRC. Карта TF2
читается целиком, 286 текстур из неё и из репозитория GMod декодируются, DXT совпадает с
декодером ImageMagick с точностью до округления.

## Открыть карту

**На Windows:** скачайте `opengmod.exe` со страницы
[Releases](https://github.com/Falcon-9FT/openGMod/releases) и запустите двойным щелчком —
откроется `gm_construct`. Другая карта — `opengmod.exe gm_flatgrass` или файл `.bsp`,
перетащенный на программу.

Управление: щелчок по окну — мышь управляет взглядом, Esc — отпустить мышь; W A S D — полёт,
пробел и Ctrl — вверх и вниз, Shift — быстрее; F11 — во весь экран; F12 — снимок экрана в
`~/.opengmod/screenshots`.

```sh
cargo run --release -p gmod-app                       # gm_construct
cargo run --release -p gmod-app -- gm_flatgrass --gmod "D:\SteamLibrary\steamapps\common\GarrysMod"
```

Видно мир, брашевые энтити и статические пропы с текстурами, световыми картами и запечённым
светом пропов, смешивание текстур на рельефе, прозрачность, воду и небо. Пока нет моделей
энтити, декалей, 3D-скайбокса, тумана, отражений и карт нормалей.

## Проверить свою установку

**Проще всего на Windows:**
1. Скачайте `gmod-check.exe` со страницы [Releases](https://github.com/Falcon-9FT/openGMod/releases).
2. Запустите его двойным щелчком (или перетащите на него папку `GarrysMod`).
3. Отчёт сохранится рядом с программой в `gmod-check-report.txt`.

Из исходников нужен [Rust](https://rustup.rs) 1.88 или новее:

```sh
cargo run --release -p gmod-check                     # найдёт GMod через Steam и проверит всё
cargo run --release -p gmod-check -- --gmod "D:\SteamLibrary\steamapps\common\GarrysMod"
cargo run --release -p gmod-check -- info             # что смонтировано и в каком порядке
cargo run --release -p gmod-check -- ls lua/autorun   # папка игровой файловой системы
cargo run --release -p gmod-check -- find "maps/*.bsp"
cargo run --release -p gmod-check -- glua lua/includes/init.lua   # Lua так, как его увидит LuaJIT
cargo run --release -p gmod-check -- --crc            # плюс проверка CRC каждого файла архивов
cargo run --release -p gmod-check -- render gm_construct          # картинка карты в gm_construct.png
cargo run --release -p gmod-check -- render gm_flatgrass --from 0,0,200 --to 800,0,100 --size 1920x1080
```

`gmod-check` читает каждый файл, для которого в openGMOD уже есть читатель (архивы,
KeyValues, весь Lua, текстуры, материалы, модели и карты, включая аддоны воркшопа), и печатает отчёт со
списком ошибок. Если что-то не прочиталось, пришлите отчёт — по нему чинятся читатели. Отдельно
отчёт перечисляет, чего не хватает в самом контенте: текстуры, на которые ссылаются материалы, и
материалы и модели, на которые ссылаются карты (обычно — из игр, которые не подключены). Старые сжатые загрузки воркшопа
(`.bin`) распаковываются в `~/.opengmod/cache/workshop`, а не в папку игры (`--no-unpack`
отключает распаковку).

`render` рисует карту на процессоре и сохраняет PNG — то же, что видно в `opengmod`, но без
видеокарты: для проверки и для машин без DirectX 12 и Vulkan. Без `--from` камера встаёт на
точку появления игрока.

## Сборка и тесты

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Внешние зависимости есть только у рендера и окна (`gmod-render`, `gmod-app`): wgpu и winit.
Остальные crate'ы обходятся стандартной библиотекой. Lua-рантайм и физика добавят mlua
(LuaJIT) и Jolt (C++, собирается через CMake).

## Устройство репозитория

```
openGMod/
├── crates/
│   ├── gmod-fs/     установка, VPK, GMA, zip, LZMA, воркшоп, файловая система, папка пользователя
│   ├── gmod-kv/     KeyValues
│   ├── gmod-glua/   диалект Lua из GMod → LuaJIT
│   ├── gmod-bsp/    карты BSP и сетка мира из них
│   ├── gmod-material/ текстуры VTF и материалы VMT
│   ├── gmod-model/  модели MDL, VVD, VTX
│   ├── gmod-world/  карта, готовая к отрисовке
│   ├── gmod-render/ рендер на wgpu (DirectX 12, Vulkan)
│   └── gmod-app/    программа opengmod
├── tools/
│   └── gmod-check/  проверка установки и просмотр игровой файловой системы
├── docs/            архитектура, физика, дорожная карта, форматы
└── scripts/         вспомогательные скрипты разработки
```

## Документация

| Документ | О чём |
|---|---|
| [Архитектура](docs/ARCHITECTURE.md) | цели, принципы, ключевые решения (язык, рендер, Lua, сеть), подсистемы |
| [Дорожная карта](docs/ROADMAP.md) | этапы от чтения контента до мультиплеера |
| [Форматы](docs/FORMATS.md) | VPK, GMA, KeyValues, `gameinfo.txt`, воркшоп, LZMA, диалект Lua |

## Лицензия

openGMOD распространяется под [лицензией MIT](LICENSE). Garry's Mod — товарный знак Facepunch
Studios, Source — товарный знак Valve. openGMOD — независимый проект, с ними не связан.
