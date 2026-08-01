#!/usr/bin/env python3
"""Exact, dependency-free 9P2000 wire oracle for the M7 Linux namespace.

The native M7 client uses a 128-byte bounded-call view, but the 9P message size
in the first four bytes remains authoritative.  This oracle deliberately tests
the inner messages without importing any repository implementation.
"""

from __future__ import annotations

import struct


MSIZE = 128
NOTAG = 0xFFFF
NOFID = 0xFFFFFFFF
QTDIR = 0x80
OREAD = 0
OWRITE = 1

TVERSION = 100
RVERSION = 101
TATTACH = 104
RATTACH = 105
RERROR = 107
TWALK = 110
RWALK = 111
TOPEN = 112
ROPEN = 113
TREAD = 116
RREAD = 117
TCLUNK = 120
RCLUNK = 121

STDOUT = b"o-core linux stdout\n"
STDERR = b"o-core linux stderr\n"


def p16(value: int) -> bytes:
    return struct.pack("<H", value)


def p32(value: int) -> bytes:
    return struct.pack("<I", value)


def p64(value: int) -> bytes:
    return struct.pack("<Q", value)


def string(value: bytes) -> bytes:
    if len(value) > 0xFFFF:
        raise ValueError("9P string is too long")
    return p16(len(value)) + value


def message(kind: int, tag: int, body: bytes = b"") -> bytes:
    packet = p32(7 + len(body)) + bytes((kind,)) + p16(tag) + body
    if len(packet) > MSIZE:
        raise ValueError("message exceeds negotiated msize")
    return packet


def qid(kind: int, version: int, path: int) -> bytes:
    return bytes((kind,)) + p32(version) + p64(path)


def root_qid(generation: int) -> bytes:
    return qid(QTDIR, generation, 1)


def srv_qid(generation: int) -> bytes:
    return qid(QTDIR, generation, 2)


def linux_qid(generation: int) -> bytes:
    return qid(QTDIR, generation, 3)


def status_qid(generation: int) -> bytes:
    return qid(0, generation, 4)


def tversion(tag: int, version: bytes) -> bytes:
    return message(TVERSION, tag, p32(MSIZE) + string(version))


def rversion(tag: int, version: bytes) -> bytes:
    return message(RVERSION, tag, p32(MSIZE) + string(version))


def tattach(tag: int) -> bytes:
    return message(TATTACH, tag, p32(1) + p32(NOFID) + string(b"") + string(b""))


def rattach(tag: int, generation: int) -> bytes:
    return message(RATTACH, tag, root_qid(generation))


def twalk(tag: int, names: tuple[bytes, ...]) -> bytes:
    return message(
        TWALK,
        tag,
        p32(1) + p32(2) + p16(len(names)) + b"".join(map(string, names)),
    )


def rwalk(tag: int, generation: int) -> bytes:
    return message(
        RWALK,
        tag,
        p16(3)
        + srv_qid(generation)
        + linux_qid(generation)
        + status_qid(generation),
    )


def topen(tag: int, mode: int) -> bytes:
    return message(TOPEN, tag, p32(2) + bytes((mode,)))


def ropen(tag: int, generation: int) -> bytes:
    return message(ROPEN, tag, status_qid(generation) + p32(MSIZE))


def tread(tag: int, count: int) -> bytes:
    # Tread itself fits in msize even when its requested reply would not.
    return message(TREAD, tag, p32(2) + p64(0) + p32(count))


def rread(tag: int, data: bytes) -> bytes:
    return message(RREAD, tag, p32(len(data)) + data)


def tclunk(tag: int) -> bytes:
    return message(TCLUNK, tag, p32(2))


def rerror(tag: int, name: bytes) -> bytes:
    return message(RERROR, tag, string(name))


def header(packet: bytes, kind: int, tag: int) -> bytes:
    if len(packet) < 7:
        raise ValueError("truncated 9P header")
    size, actual_kind, actual_tag = struct.unpack_from("<IBH", packet)
    if size != len(packet):
        raise ValueError("9P size does not match the packet")
    if size > MSIZE:
        raise ValueError("9P packet exceeds msize")
    if actual_kind != kind:
        raise ValueError("wrong 9P response type")
    if actual_tag != tag:
        raise ValueError("9P response tag was not echoed")
    return packet[7:]


def counted(data: bytes, offset: int = 0) -> tuple[bytes, int]:
    if offset + 2 > len(data):
        raise ValueError("truncated 9P string length")
    length = struct.unpack_from("<H", data, offset)[0]
    end = offset + 2 + length
    if end > len(data):
        raise ValueError("truncated 9P string")
    return data[offset + 2 : end], end


def expect_error(packet: bytes, tag: int, name: bytes) -> None:
    body = header(packet, RERROR, tag)
    actual, end = counted(body)
    if actual != name or end != len(body):
        raise ValueError("wrong or non-canonical Rerror")


def expect_version(packet: bytes, tag: int, version: bytes) -> None:
    body = header(packet, RVERSION, tag)
    if len(body) < 6 or struct.unpack_from("<I", body)[0] != MSIZE:
        raise ValueError("wrong Rversion msize")
    actual, end = counted(body, 4)
    if actual != version or end != len(body):
        raise ValueError("wrong or non-canonical Rversion")


def expect_tversion(packet: bytes, version: bytes) -> None:
    body = header(packet, TVERSION, NOTAG)
    if len(body) < 6 or struct.unpack_from("<I", body)[0] != MSIZE:
        raise ValueError("wrong Tversion msize")
    actual, end = counted(body, 4)
    if actual != version or end != len(body):
        raise ValueError("wrong or non-canonical Tversion")


def expect_read(packet: bytes, tag: int, requested: int, expected: bytes) -> None:
    body = header(packet, RREAD, tag)
    if len(body) < 4:
        raise ValueError("truncated Rread")
    count = struct.unpack_from("<I", body)[0]
    data = body[4:]
    if count != len(data) or count > requested or data != expected:
        raise ValueError("invalid Rread count or payload")


def rejected(callable_: object, *args: object) -> None:
    try:
        callable_(*args)  # type: ignore[operator]
    except ValueError:
        return
    raise AssertionError("wire mutant was accepted")


def main() -> None:
    assert len(STDOUT) == len(STDERR) == 20

    requests = (
        tattach(1),
        tversion(NOTAG, b"9P2000.u"),
        tversion(NOTAG, b"9P2000"),
        tattach(4),
        twalk(5, (b"srv", b"linux", b"missing")),
        twalk(6, (b"srv", b"linux", b"status")),
        topen(7, OWRITE),
        topen(8, OREAD),
        tread(9, MSIZE + 1),
        tread(10, 20),
        tclunk(11),
    )
    exact_request_hex = (
        "1300000068010001000000ffffffff00000000",
        "1500000064ffff8000000008003950323030302e75",
        "1300000064ffff800000000600395032303030",
        "1300000068040001000000ffffffff00000000",
        "260000006e050001000000020000000300030073727605006c696e757807006d697373696e67",
        "250000006e060001000000020000000300030073727605006c696e75780600737461747573",
        "0c0000007007000200000001",
        "0c0000007008000200000000",
        "1700000074090002000000000000000000000081000000",
        "17000000740a0002000000000000000000000014000000",
        "0b000000780b0002000000",
    )
    assert tuple(packet.hex() for packet in requests) == exact_request_hex
    expect_tversion(requests[1], b"9P2000.u")
    expect_tversion(requests[2], b"9P2000")

    responses = (
        rerror(1, b"sequence"),
        rversion(NOTAG, b"unknown"),
        rversion(NOTAG, b"9P2000"),
        rattach(4, 1),
        rerror(5, b"path"),
        rwalk(6, 1),
        rerror(7, b"mode"),
        ropen(8, 1),
        rerror(9, b"count"),
        rread(10, STDOUT),
        message(RCLUNK, 11),
    )
    exact_response_hex = (
        "110000006b0100080073657175656e6365",
        "1400000065ffff800000000700756e6b6e6f776e",
        "1300000065ffff800000000600395032303030",
        "1400000069040080010000000100000000000000",
        "0d0000006b0500040070617468",
        "300000006f06000300800100000002000000000000008001000000030000000000000000010000000400000000000000",
        "0d0000006b070004006d6f6465",
        "180000007108000001000000040000000000000080000000",
        "0e0000006b09000500636f756e74",
        "1f000000750a00140000006f2d636f7265206c696e7578207374646f75740a",
        "07000000790b00",
    )
    assert tuple(packet.hex() for packet in responses) == exact_response_hex
    expect_error(responses[0], 1, b"sequence")
    expect_version(responses[1], NOTAG, b"unknown")
    expect_version(responses[2], NOTAG, b"9P2000")
    assert header(responses[3], RATTACH, 4) == root_qid(1)
    expect_error(responses[4], 5, b"path")
    assert header(responses[5], RWALK, 6) == (
        p16(3) + srv_qid(1) + linux_qid(1) + status_qid(1)
    )
    expect_error(responses[6], 7, b"mode")
    assert header(responses[7], ROPEN, 8) == status_qid(1) + p32(MSIZE)
    expect_error(responses[8], 9, b"count")
    expect_read(responses[9], 10, 20, STDOUT)
    assert header(responses[10], RCLUNK, 11) == b""

    # A fresh generation negotiates independently and exposes only its exact
    # generation payload through the same namespace and fid lifecycle.
    generation_two_requests = (
        tversion(NOTAG, b"9P2000"),
        tattach(2),
        twalk(3, (b"srv", b"linux", b"status")),
        topen(4, OREAD),
        tread(5, 20),
        tclunk(6),
    )
    assert tuple(packet.hex() for packet in generation_two_requests) == (
        "1300000064ffff800000000600395032303030",
        "1300000068020001000000ffffffff00000000",
        "250000006e030001000000020000000300030073727605006c696e75780600737461747573",
        "0c0000007004000200000000",
        "1700000074050002000000000000000000000014000000",
        "0b00000078060002000000",
    )
    generation_two_responses = (
        rversion(NOTAG, b"9P2000"),
        rattach(2, 2),
        rwalk(3, 2),
        ropen(4, 2),
        rread(5, STDERR),
        message(RCLUNK, 6),
    )
    assert tuple(packet.hex() for packet in generation_two_responses) == (
        "1300000065ffff800000000600395032303030",
        "1400000069020080020000000100000000000000",
        "300000006f03000300800200000002000000000000008002000000030000000000000000020000000400000000000000",
        "180000007104000002000000040000000000000080000000",
        "1f000000750500140000006f2d636f7265206c696e7578207374646572720a",
        "07000000790600",
    )
    expect_version(generation_two_responses[0], NOTAG, b"9P2000")
    assert header(generation_two_responses[1], RATTACH, 2) == root_qid(2)
    assert header(generation_two_responses[2], RWALK, 3)[-13:] == status_qid(2)
    assert header(generation_two_responses[3], ROPEN, 4) == (
        status_qid(2) + p32(MSIZE)
    )
    expect_read(generation_two_responses[4], 5, 20, STDERR)
    assert header(generation_two_responses[5], RCLUNK, 6) == b""

    size_mutant = bytearray(responses[9])
    size_mutant[0] -= 1
    rejected(expect_read, bytes(size_mutant), 10, 20, STDOUT)
    rejected(expect_read, responses[9], 9, 20, STDOUT)
    rejected(expect_read, responses[9], 10, 19, STDOUT)
    truncated_string = rerror(1, b"sequence")[:-1]
    rejected(expect_error, truncated_string, 1, b"sequence")
    wrong_type = bytearray(responses[0])
    wrong_type[4] = RVERSION
    rejected(expect_error, bytes(wrong_type), 1, b"sequence")
    wrong_version_tag = bytearray(requests[1])
    wrong_version_tag[5:7] = p16(2)
    rejected(expect_tversion, bytes(wrong_version_tag), b"9P2000.u")
    rejected(message, RREAD, 1, p32(MSIZE) + bytes(MSIZE))

    print(
        "M7 9P2000 Linux namespace wire oracle: PASS "
        "(11 exact requests, 2 generations, 7 mutant rejections)"
    )


if __name__ == "__main__":
    main()
