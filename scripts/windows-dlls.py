"""Copy the DLLs a set of Windows programs needs, and the DLLs those need, from a GTK
build's bin folder into a destination folder. System DLLs (not found there) are skipped.

usage: python windows-dlls.py <gtk bin dir> <dest dir> <exe or dll>...
"""

import shutil
import sys
from pathlib import Path

import pefile


def imports(path: Path) -> list[str]:
    pe = pefile.PE(str(path), fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"]]
    )
    names = [e.dll.decode() for e in getattr(pe, "DIRECTORY_ENTRY_IMPORT", [])]
    pe.close()
    return names


def main() -> None:
    src, dest, roots = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3:]
    available = {p.name.lower(): p for p in src.glob("*.dll")}
    seen: set[str] = set()
    todo = [Path(r) for r in roots]
    while todo:
        for name in imports(todo.pop()):
            key = name.lower()
            if key in seen or key not in available:
                continue
            seen.add(key)
            shutil.copy2(available[key], dest / available[key].name)
            todo.append(available[key])
    print(f"copied {len(seen)} DLLs: {', '.join(sorted(seen))}")


if __name__ == "__main__":
    main()
