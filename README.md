# openGMOD

**openGMOD** — реимплементация движка Garry's Mod с нуля на Rust, по примеру
[OpenMW](https://openmw.org) и [openOMSI](https://github.com/turbo-devv/openOMSI). Цель — 64-битный
многопоточный движок с современным рендером (wgpu: Vulkan / DirectX 12), на котором
работают контент, Lua-аддоны, геймоды и воркшоп Garry's Mod без изменений.

> [!IMPORTANT]
> **Нужна лицензионная Garry's Mod.** В openGMOD нет игрового контента: он работает на файлах
> установленной игры и на аддонах воркшопа, на которые подписан её владелец. **Папку игры
> openGMOD только читает** — ничего в ней не создаёт и не меняет.

> [!WARNING]
> **Ранняя стадия.** Готов фундамент — чтение контента. Запустить игру пока нельзя; что дальше,
> см. в [дорожной карте](docs/ROADMAP.md).

## Что уже есть

| Crate | Что делает |
|---|---|
| [`gmod-fs`](crates/gmod-fs) | находит GMod (указанная папка, `GMOD_ROOT`, библиотеки Steam), читает VPK и GMA и собирает файловую систему так же, как GMod: `gameinfo.txt`, подписки воркшопа (включая старые сжатые `.bin`), аддоны из `garrysmod/addons`, игры из `cfg/mount.cfg` |
| [`gmod-kv`](crates/gmod-kv) | KeyValues: материалы `.vmt`, скрипты, VGUI `.res`, локализация, файлы Steam |
| [`gmod-glua`](crates/gmod-glua) | диалект Lua из GMod (`!=`, `!`, `&&`, `\|\|`, `//`, `/* */`, `continue`) → обычный LuaJIT, строка в строку |
| [`gmod-check`](tools/gmod-check) | проверка вашей установки: что openGMOD уже умеет прочитать |

Проверено на настоящих данных. Все 596 Lua-файлов GMod после перевода компилируются в LuaJIT
2.0 и 2.1. Тестовые VPK, собранные инструментами Valve, читаются с совпадающими CRC.

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
```

`gmod-check` читает каждый файл, для которого в openGMOD уже есть читатель (архивы,
KeyValues, весь Lua, включая аддоны воркшопа), и печатает отчёт со списком ошибок. Если что-то
не прочиталось, пришлите отчёт — по нему чинятся читатели. Старые сжатые загрузки воркшопа
(`.bin`) распаковываются в `~/.opengmod/cache/workshop`, а не в папку игры (`--no-unpack`
отключает распаковку).

## Сборка и тесты

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Пока у crate'ов нет внешних зависимостей. Рендер, Lua-рантайм и физика добавят wgpu, mlua
(LuaJIT) и Jolt (C++, собирается через CMake).

## Устройство репозитория

```
openGMod/
├── crates/
│   ├── gmod-fs/     установка, VPK, GMA, LZMA, воркшоп, файловая система, папка пользователя
│   ├── gmod-kv/     KeyValues
│   └── gmod-glua/   диалект Lua из GMod → LuaJIT
├── tools/
│   └── gmod-check/  проверка установки и просмотр игровой файловой системы
├── docs/            архитектура, физика, дорожная карта, форматы
└── scripts/         вспомогательные скрипты разработки
```

## Документация

| Документ | О чём |
|---|---|
| [Архитектура](docs/ARCHITECTURE.md) | цели, принципы, ключевые решения (язык, рендер, Lua, сеть), подсистемы |
| [Физика](docs/PHYSICS.md) | что на самом деле «открыто» у физики Source, почему Jolt, как констрейнты GMod ложатся на Jolt |
| [Дорожная карта](docs/ROADMAP.md) | этапы от чтения контента до мультиплеера |
| [Форматы](docs/FORMATS.md) | VPK, GMA, KeyValues, `gameinfo.txt`, воркшоп, LZMA, диалект Lua |
| [Как участвовать](CONTRIBUTING.md) | правила чистой реализации: какой код и контент нельзя трогать |

## Лицензия

openGMOD распространяется под [лицензией MIT](LICENSE). Garry's Mod — товарный знак Facepunch
Studios, Source — товарный знак Valve. openGMOD — независимый проект, с ними не связан.
