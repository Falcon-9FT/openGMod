# Физика

## Что на самом деле «открыто»

Физика Source — это **IVP** (Ipion Virtual Physics). Valve купила его вместе с компанией Ipion и
обернула в **VPhysics** (`vphysics.dll`). Вот что из этого и вокруг этого доступно на
октябрь 2026 года.

| Что | Где и в каком виде | Можно ли использовать в openGMOD |
|---|---|---|
| Исходники IVP | Официально не публиковались никогда. В сети лежат утечка Half-Life 2 (2003) и код, «частично восстановленный из ассемблера» (`Source-Authors/source-physics`, `theivpdocs/ivp-source1`, `nillerusr/source-physics`); в заголовках — «Copyright Ipion Software GmbH, All rights reserved», лицензии нет | **Нет.** Это и есть «физика Source в открытом доступе». Такой код делает незаконным любой проект, который его касается; OpenMW по той же причине не принимает утёкший код. Facepunch отклонила просьбу перевести GMod на этот код |
| Исходники VPhysics | Только утечка CS:GO/TF2 (2020) | **Нет** |
| `vphysics.dll` | Закрытые бинарники, Valve обновляет их до сих пор (сборки 2025–2026) | **Нет** |
| Source SDK 2013, включая TF2 SDK (февраль 2025) | Открыт на GitHub, но физики в нём нет: только интерфейсы (`vphysics_interface.h`) и игровой код, который их вызывает | **Нет.** Лицензия: «use, copy, and modify the SDK to develop a modified Valve game running on Valve's Source 1 engine», и только бесплатно. Движок-замена под MIT под это не подходит |
| [VPhysics-Jolt](https://github.com/misyltoad/VPhysics-Jolt) | Замена `vphysics.dll` на Jolt; работает с GMod (64 бита); лицензия MIT | **Как справочник.** Показывает, как поведение Source ложится на Jolt: единицы, демпфирование, контроллеры, рэгдоллы. Целиком не переносится: проект собирается против заголовков Source SDK, а интерфейс VPhysics, нужный GMod (из CS:GO), его авторы распространять не могут. Ломающихся констрейнтов и raycast-машин в нём нет |
| [Jolt Physics](https://github.com/jrouwe/JoltPhysics) 5.6 | MIT; Horizon Forbidden West, Death Stranding 2, физика по умолчанию в Godot 4.4+ | **Да — выбран** |
| [Rapier](https://rapier.rs) 0.36 | Чистый Rust, Apache-2.0 | Да — запасной вариант |

**Итог:** открытой физики Source нет, есть утечки, и их не трогаем. Поведение физики GMod
воспроизводим заново на Jolt, сверяясь с настоящей игрой и с идеями VPhysics-Jolt.

## Почему Jolt

* **Всё нужное есть сразу.** Констрейнты GMod — это энтити движка, которые создаёт
  `lua/includes/modules/constraint.lua`. У каждого есть пара в Jolt:

  | Инструмент GMod | Энтити движка | Jolt |
  |---|---|---|
  | Weld | `phys_constraint` | `FixedConstraint` + предел силы (ломается) |
  | Axis | `phys_hinge` | `HingeConstraint` с трением и пределами |
  | Ballsocket | `phys_ballsocket` | `PointConstraint` |
  | Advanced Ballsocket | `phys_ragdollconstraint` | `SixDOFConstraint` / `SwingTwistConstraint` |
  | Slider | `phys_slideconstraint` | `SliderConstraint` |
  | Rope | `phys_lengthconstraint` + `keyframe_rope` | `DistanceConstraint` (мин./макс. длина) |
  | Elastic, Hydraulic, Muscle, Winch | `phys_spring` + `gmod_winch_controller` | `DistanceConstraint` с пружиной; длину меняет контроллер |
  | Pulley | `phys_pulleyconstraint` | `PulleyConstraint` |
  | Motor, Wheel | `phys_torque` | мотор `HingeConstraint` |
  | Keepupright | `phys_keepupright` | угловой предел `SixDOFConstraint` |
  | No-Collide | `logic_collision_pair` | фильтр пар (`GroupFilter`) |
  | (группы констрейнтов) | `phys_constraintsystem` | общий остров решателя |

* **Плавучесть.** Пропы в GMod плавают: это делает «fluid controller» IVP. У Jolt есть готовая
  `Body::ApplyBuoyancyImpulse`, у Rapier ничего такого нет.
* **Шкив** (`PulleyConstraint`) есть в Jolt и отсутствует в Rapier.
* **Машины.** `WheeledVehicleController` в Jolt — основа для `prop_vehicle_jeep` и
  `prop_vehicle_airboat`; логику управления Source пишем сами.
* **Доказано на GMod.** VPhysics-Jolt запускает настоящий GMod на Jolt, и постройки игроков
  работают.
* **Масштаб.** Сервера GMod держат тысячи пропов и констрейнтов, а Jolt многопоточный и
  рассчитан ровно на это.

**Цена:** Jolt написан на C++. Готовые Rust-обёртки устарели (`rolt` 0.3.1 — это Jolt 5.0,
2024 год). Поэтому будет свой небольшой crate `gmod-physics-sys` поверх C API Jolt
([joltc](https://github.com/amerkoleci/joltc), MIT, следит за Jolt 5.6), собираемый через CMake.
Остальной движок видит только `gmod-physics` с семантикой GMod (`PhysObj`, констрейнты,
трассировки), поэтому замена движка физики — не катастрофа. Rapier остаётся запасным
вариантом: на нём сборка проще, но шкив и плавучесть придётся писать самим.

## Что делает не физический движок

* **Движение игрока.** Как в Source, это игровой код, а не физика: трение, ускорение на земле
  и в воздухе, ступеньки, лестницы, вода, noclip. Хуки GMod `SetupMove`, `Move`, `FinishMove` и
  `CMoveData` требуют, чтобы он был отдельным и детерминированным.
* **Трассировки** `util.TraceLine` / `util.TraceHull`: против мира — по брашам BSP (своё
  дерево, тот же эпсилон, что у Source); против энтити — по их физическим формам.
* **Физган и гравиган** — «shadow controller»: тело тянется к цели с ограничением скорости и
  ускорения. Это пишем сами поверх Jolt.
* **Формы столкновений:**
  * модели — выпуклые оболочки из `.phy`; их двоичный формат описан сообществом;
  * мир — браши BSP и треугольники дисплейсментов;
  * масса, трение, упругость и плотность — из `scripts/surfaceproperties*.txt` (KeyValues,
    уже читаются `gmod-kv`).

## Единицы

* Source считает в дюймах (1 юнит = 0,0254 м), Jolt — в метрах. Перевод делается на границе
  `gmod-physics`, Lua видит юниты Source.
* Гравитация `sv_gravity` 600 юнитов/с² = 15,24 м/с²; масса — в килограммах в обоих мирах.

## Что сверить с настоящей игрой

Это делается «чёрным ящиком» — измерения в GMod, без чужого кода:

* демпфирование по умолчанию;
* поведение сварки под нагрузкой (насколько «мягкие» weld-ы);
* предел скорости тел;
* сила трения;
* как ломаются констрейнты;
* плотность воды;
* как сон и пробуждение тел ведут себя на краях.

Для таких измерений пригодится мост к живой GMod (сценарии на Lua, которые пишут результаты в
файл).
