#!/usr/bin/env python3
"""Rebuilds the lists openGMOD keeps of GMod's Lua API from the GMod wiki.

    scripts/wiki-api.py /path/to/glua-api-snippets/library

The input is the wiki as Lua definitions: the `lua-language-server-addon` branch of
https://github.com/luttje/glua-api-snippets (MIT), generated from wiki.facepunch.com/gmod.
Two files come out:

* crates/gmod-lua/data/enums.txt - every enumeration and its value (`ACT_IDLE 1`,
  `SENSORBONE.HIP 0`): the engine defines them, the game's Lua uses them.
* tools/gmod-check/data/api.txt - every function of the engine's libraries and types with
  the realms it exists in (`SC net.WriteBit`: server and client), for `gmod-check lua` to
  count how much of the API is there. Hooks (`GM:`, `ENTITY:` ...) and Derma panels, which
  are written in Lua, are left out.
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# the types of the engine: their methods are found with FindMetaTable
ENGINE_TYPES = """
    Angle bf_read CEffectData CLuaEmitter CLuaLocomotion CLuaParticle CMoveData CNavArea
    CNavLadder CNewParticleEffect Color ConVar CRecipientFilter CSEnt CSoundPatch
    CTakeDamageInfo CUserCmd Entity File IGModAudioChannel IMaterial IMesh IRestore ISave
    ITexture IVideoWriter NextBot NPC Panel PathFollower PhysCollide PhysObj Player
    ProjectedTexture SurfaceInfo Vector Vehicle VMatrix Weapon
""".split()

REALMS = {
    "Shared": "SC",
    "Server": "S",
    "Client": "C",
    "Menu": "M",
    "Client and menu": "CM",
    "Shared and Menu": "SCM",
}


def enums(library: Path) -> list[str]:
    out = []
    table = None
    for line in (library / "enums.lua").read_text(encoding="utf-8").splitlines():
        if m := re.match(r"^([A-Za-z_0-9]+) = \{$", line):
            table = m.group(1)
        elif line == "}":
            table = None
        elif m := re.match(r"^\s*([A-Za-z_0-9]+) = (-?\d+),?$", line):
            name = f"{table}.{m.group(1)}" if table else m.group(1)
            out.append(f"{name} {m.group(2)}")
    return out


def api(library: Path) -> list[str]:
    out = set()
    for file in sorted(library.glob("*.lua")):
        realm = None
        for line in file.read_text(encoding="utf-8").splitlines():
            if m := re.match(r"^---!\[\((.+?)\)\]", line):
                realm = REALMS.get(m.group(1))
            elif m := re.match(r"^function ([A-Za-z_0-9]+)([.:])([A-Za-z_0-9]+)\(", line):
                owner, sep, name = m.groups()
                if realm and (sep == "." or owner in ENGINE_TYPES):
                    out.add(f"{realm} {owner}{sep}{name}")
                realm = None
    return sorted(out, key=lambda entry: entry.split(" ", 1)[1].lower())


def write(path: Path, header: str, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(header + "\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"{path.relative_to(ROOT)}: {len(lines)} entries")


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    library = Path(sys.argv[1])
    source = "# From the GMod wiki (wiki.facepunch.com/gmod) by way of glua-api-snippets; scripts/wiki-api.py rebuilds it.\n"
    write(ROOT / "crates/gmod-lua/data/enums.txt", source + "# NAME value\n", enums(library))
    write(
        ROOT / "tools/gmod-check/data/api.txt",
        source + "# realms (S server, C client, M menu) and name\n",
        api(library),
    )


if __name__ == "__main__":
    main()
