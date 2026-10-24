"""Tests of crashdump's PE id reading and Breakpad symbol lookup (no dump tools needed).

python3 -m pytest scripts/tests
"""

from __future__ import annotations

import struct
import sys
from importlib.machinery import SourceFileLoader
from pathlib import Path
from types import ModuleType


def _load() -> ModuleType:
    path = Path(__file__).resolve().parent.parent / "crashdump"
    module = ModuleType("crashdump")
    module.__file__ = str(path)
    sys.modules["crashdump"] = module
    SourceFileLoader("crashdump", str(path)).exec_module(module)
    return module


crashdump = _load()

GUID = (0x8CFE1CF4, 0x25EE, 0xA3D3, bytes.fromhex("4C4C44205044422E"))


def _pe(stamp: int, image_size: int, pdb: bytes, age: int = 1) -> bytes:
    """Return a minimal PE32+ image: one section holding a debug directory and its RSDS."""
    data = bytearray(0x400)
    data[:2] = b"MZ"
    pe = 0x80
    struct.pack_into("<I", data, 0x3C, pe)
    data[pe : pe + 4] = b"PE\0\0"
    coff = pe + 4
    optional_size = 240
    struct.pack_into("<HHI", data, coff, 0x8664, 1, stamp)
    struct.pack_into("<H", data, coff + 16, optional_size)
    optional = coff + 20
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<I", data, optional + 56, image_size)
    section_rva, section_raw = 0x1000, 0x200
    debug_rva = section_rva
    struct.pack_into("<II", data, optional + 112 + 8 * 6, debug_rva, 28)
    table = optional + optional_size
    struct.pack_into("<IIII", data, table + 8, 0x200, section_rva, 0x200, section_raw)
    rsds = section_raw + 28
    record = b"RSDS" + struct.pack("<IHH", *GUID[:3]) + GUID[3] + struct.pack("<I", age) + pdb
    struct.pack_into("<IIII", data, section_raw + 12, 2, len(record), 0, rsds)
    data[rsds : rsds + len(record)] = record
    return bytes(data)


def test_pe_ids_are_spelled_as_breakpad_does() -> None:
    ids = crashdump.pe_ids(_pe(0x6ADC8F25, 0x288000, b"C:\\b\\uniloader.pdb\0"))
    assert ids is not None
    assert ids.code_id == "6ADC8F25288000"
    assert ids.debug_file == "uniloader.pdb"
    assert ids.debug_id == "8CFE1CF425EEA3D34C4C44205044422E1"


def test_not_a_pe_gives_nothing() -> None:
    assert crashdump.pe_ids(b"") is None
    assert crashdump.pe_ids(b"MZ" + b"\0" * 100) is None
    assert crashdump.pe_ids(b"PK\x03\x04 not an image") is None


SYM = """MODULE windows x86_64 8CFE1CF425EEA3D34C4C44205044422E1 uniloader.pdb
INFO CODE_ID 6ADC8F25288000 uniloader.dll
FILE 0 src/win/crash.rs
FILE 1 src/win/report.rs
FUNC 1000 40 0 uniloader::win::crash::watch
1000 20 270 0
1020 20 275 0
FUNC m 2000 10 0 uniloader::win::report::seen
2000 10 251 1
PUBLIC 3000 0 exported_thing
STACK CFI INIT 1000 40 .cfa: $rsp 8 +
"""


def test_offsets_are_named_with_their_lines() -> None:
    symbols = crashdump.Symbols.parse(SYM)
    assert symbols.name(0x1024) == "uniloader::win::crash::watch (src/win/crash.rs:275)"
    assert symbols.name(0x2004) == "uniloader::win::report::seen (src/win/report.rs:251)"
    assert symbols.name(0x3010) == "exported_thing + 0x10"


def test_panic_frames_are_named_by_code_id(tmp_path: Path) -> None:
    sym = tmp_path / "uniloader.sym"
    sym.write_text(SYM)
    log = (
        "2026-10-24T16:51:35Z [INFO] before\n"
        "2026-10-24T16:51:35Z [ERROR] uniloader::win::panics: panic in thread t: boom at a.rs:1:2\n"
        "  at dwmapi.dll+0x1024 [6ADC8F25288000]\n"
        "  at game.exe+0x10 [11111111aaaa]\n"
        "  at 0x7ff00010\n"
        "2026-10-24T16:51:36Z [INFO] after\n"
    )
    lines = crashdump.name_panics(log, {"6adc8f25288000": sym}, {})
    assert lines == [
        "2026-10-24T16:51:35Z [ERROR] uniloader::win::panics: panic in thread t: boom at a.rs:1:2",
        "  at dwmapi.dll!uniloader::win::crash::watch (src/win/crash.rs:275)",
        "  at game.exe + 0x10",
        "  at 0x7ff00010",
    ]


def test_offsets_outside_functions_stay_unnamed() -> None:
    symbols = crashdump.Symbols.parse(SYM)
    assert symbols.name(0x0FFF) is None
    assert symbols.name(0x1040) is None
