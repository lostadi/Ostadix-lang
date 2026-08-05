#!/usr/bin/env python3
"""Independent exact-wire oracle for the bounded native M7B-1 slice.

The native transport carries one zero-padded 9P2000 message as four ordered
16-byte local-IPC fragments.  Fragmentation is transport-only: the first u32
inside the reconstructed buffer remains the authoritative 9P message size.
This oracle intentionally does not import the O-core implementation.
"""

from __future__ import annotations

import hashlib
import struct


MSIZE = 64
FRAGMENT_BYTES = 16
FRAGMENT_COUNT = 4
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
TWRITE = 118
TCLUNK = 120
RCLUNK = 121

VERSION = b"9P2000"
OBJECT_NAME = b"object"
LOGICAL_DATA = b"m7b-logical-object!\n"
LOGICAL_DIGEST = bytes.fromhex(
    "59a08e13c63eb8acdae93f4caf051307"
    "33a0f5ab24e564fb1206f0f1d055809b"
)


def p16(value: int) -> bytes:
    return struct.pack("<H", value)


def p32(value: int) -> bytes:
    return struct.pack("<I", value)


def p64(value: int) -> bytes:
    return struct.pack("<Q", value)


def string(value: bytes) -> bytes:
    return p16(len(value)) + value


def message(kind: int, tag: int, body: bytes = b"") -> bytes:
    packet = p32(7 + len(body)) + bytes((kind,)) + p16(tag) + body
    if len(packet) > MSIZE:
        raise ValueError("message exceeds the bounded M7B-1 msize")
    return packet


def fragments(packet: bytes) -> tuple[bytes, ...]:
    if len(packet) < 7 or len(packet) > MSIZE:
        raise ValueError("invalid 9P message length")
    padded = packet.ljust(MSIZE, b"\0")
    return tuple(
        padded[index : index + FRAGMENT_BYTES]
        for index in range(0, MSIZE, FRAGMENT_BYTES)
    )


def reassemble(parts: tuple[bytes, ...]) -> bytes:
    if len(parts) != FRAGMENT_COUNT or any(
        len(part) != FRAGMENT_BYTES for part in parts
    ):
        raise ValueError("invalid local transport fragment geometry")
    padded = b"".join(parts)
    size = struct.unpack_from("<I", padded)[0]
    if size < 7 or size > MSIZE or any(padded[size:]):
        raise ValueError("invalid authoritative size or nonzero padding")
    return padded[:size]


def qid(kind: int, generation: int, path: int) -> bytes:
    return bytes((kind,)) + p32(generation) + p64(path)


def tversion(tag: int = NOTAG) -> bytes:
    return message(TVERSION, tag, p32(MSIZE) + string(VERSION))


def tattach(tag: int, root_fid: int, digest: bytes = LOGICAL_DIGEST) -> bytes:
    return message(
        TATTACH,
        tag,
        p32(root_fid) + p32(NOFID) + string(b"") + string(digest),
    )


def twalk(tag: int, root_fid: int, file_fid: int) -> bytes:
    return message(
        TWALK,
        tag,
        p32(root_fid) + p32(file_fid) + p16(1) + string(OBJECT_NAME),
    )


def topen(tag: int, file_fid: int, mode: int = OREAD) -> bytes:
    return message(TOPEN, tag, p32(file_fid) + bytes((mode,)))


def tread(tag: int, file_fid: int) -> bytes:
    return message(TREAD, tag, p32(file_fid) + p64(0) + p32(len(LOGICAL_DATA)))


def twrite(tag: int, file_fid: int, payload: bytes) -> bytes:
    return message(
        TWRITE,
        tag,
        p32(file_fid) + p64(0) + p32(len(payload)) + payload,
    )


def tclunk(tag: int, file_fid: int) -> bytes:
    return message(TCLUNK, tag, p32(file_fid))


def rerror(tag: int, name: bytes) -> bytes:
    return message(RERROR, tag, string(name))


def header(packet: bytes) -> tuple[int, int, bytes]:
    if len(packet) < 7 or struct.unpack_from("<I", packet)[0] != len(packet):
        raise ValueError("malformed 9P header")
    return packet[4], struct.unpack_from("<H", packet, 5)[0], packet[7:]


class Provider:
    """Tiny independent provider oracle with provider-local fid state."""

    def __init__(self, ordinal: int, generation: int, available: bool) -> None:
        self.ordinal = ordinal
        self.generation = generation
        self.available = available
        self.state = "unversioned"
        self.root_fid = 0
        self.file_fid = 0
        self.read_failed = False

    def path(self, leaf: int) -> int:
        return (self.ordinal << 32) | leaf

    def handle(self, packet: bytes) -> bytes:
        kind, tag, body = header(packet)
        if kind == TVERSION and self.state == "unversioned":
            if body != p32(MSIZE) + string(VERSION):
                return rerror(tag, b"version")
            self.state = "versioned"
            return message(RVERSION, tag, p32(MSIZE) + string(VERSION))

        if kind == TATTACH and self.state == "versioned":
            if len(body) != 44:
                return rerror(tag, b"sequence")
            root_fid, afid = struct.unpack_from("<II", body)
            uname_length = struct.unpack_from("<H", body, 8)[0]
            digest_length = struct.unpack_from("<H", body, 10)[0]
            if (
                root_fid in (0, NOFID)
                or afid != NOFID
                or uname_length != 0
                or digest_length != len(LOGICAL_DIGEST)
                or body[12:] != LOGICAL_DIGEST
            ):
                return rerror(tag, b"digest")
            self.root_fid = root_fid
            self.state = "attached"
            return message(
                RATTACH,
                tag,
                qid(QTDIR, self.generation, self.path(1)),
            )

        if kind == TWALK and self.state == "attached":
            if len(body) < 12:
                return rerror(tag, b"path")
            root_fid, file_fid, names = struct.unpack_from("<IIH", body)
            if (
                root_fid != self.root_fid
                or file_fid in (0, NOFID, root_fid)
                or names != 1
                or body[10:] != string(OBJECT_NAME)
            ):
                return rerror(tag, b"path")
            self.file_fid = file_fid
            self.state = "walked"
            return message(
                RWALK,
                tag,
                p16(1) + qid(0, self.generation, self.path(2)),
            )

        if kind == TOPEN and self.state == "walked":
            if body != p32(self.file_fid) + bytes((OREAD,)):
                return rerror(tag, b"mode")
            self.state = "opened"
            return message(
                ROPEN,
                tag,
                qid(0, self.generation, self.path(2)) + p32(MSIZE),
            )

        if kind == TREAD and self.state == "opened":
            expected = p32(self.file_fid) + p64(0) + p32(len(LOGICAL_DATA))
            if body != expected:
                return rerror(tag, b"count")
            if not self.available:
                self.read_failed = True
                self.state = "fault-ready"
                return rerror(tag, b"unavail")
            self.state = "read"
            return message(RREAD, tag, p32(len(LOGICAL_DATA)) + LOGICAL_DATA)

        if kind == TCLUNK and self.state == "read":
            if body != p32(self.file_fid):
                return rerror(tag, b"sequence")
            self.state = "clunked"
            return message(RCLUNK, tag)

        return rerror(tag, b"sequence")

    def fault(self) -> None:
        if self.state != "fault-ready" or not self.read_failed:
            raise AssertionError("provider fault occurred outside the bounded cut")
        self.state = "faulted"


class CapabilityDenied(RuntimeError):
    """Oracle equivalent of the native stale-send capability denial."""


class Binding:
    """Generation-tagged service binding for one provider endpoint."""

    def __init__(self, provider: Provider, cap_generation: int) -> None:
        self.provider = provider
        self.cap_generation = cap_generation
        self.active = True

    def send(self, token: int, packet: bytes) -> bytes:
        if not self.active or token != self.cap_generation:
            raise CapabilityDenied("stale provider capability")
        return reassemble(
            fragments(self.provider.handle(reassemble(fragments(packet))))
        )

    def withdraw(self) -> None:
        if not self.active or self.provider.state != "faulted":
            raise AssertionError("binding withdrawn before provider fault")
        self.active = False
        self.cap_generation += 1


def round_trip(binding: Binding, token: int, packet: bytes) -> bytes:
    return binding.send(token, packet)


def expect(packet: bytes, kind: int, tag: int) -> bytes:
    actual_kind, actual_tag, body = header(packet)
    if actual_kind != kind or actual_tag != tag:
        raise AssertionError((actual_kind, actual_tag, kind, tag))
    return body


def run_provider(
    binding: Binding,
    token: int,
    root_fid: int,
    file_fid: int,
    tag_base: int,
) -> bytes | None:
    provider = binding.provider
    expect(round_trip(binding, token, tversion()), RVERSION, NOTAG)
    attach = expect(
        round_trip(binding, token, tattach(tag_base, root_fid)),
        RATTACH,
        tag_base,
    )
    assert attach == qid(QTDIR, provider.generation, provider.path(1))
    walk = expect(
        round_trip(binding, token, twalk(tag_base + 1, root_fid, file_fid)),
        RWALK,
        tag_base + 1,
    )
    assert walk == p16(1) + qid(0, provider.generation, provider.path(2))
    opened = expect(
        round_trip(binding, token, topen(tag_base + 2, file_fid)),
        ROPEN,
        tag_base + 2,
    )
    assert opened == qid(0, provider.generation, provider.path(2)) + p32(MSIZE)
    read = round_trip(binding, token, tread(tag_base + 3, file_fid))
    kind, tag, body = header(read)
    assert tag == tag_base + 3
    data: bytes | None = None
    if provider.available:
        assert kind == RREAD and body == p32(len(LOGICAL_DATA)) + LOGICAL_DATA
        data = body[4:]
    else:
        assert kind == RERROR and body == string(b"unavail")
    if provider.available:
        expect(
            round_trip(binding, token, tclunk(tag_base + 4, file_fid)),
            RCLUNK,
            tag_base + 4,
        )
    return data


def main() -> None:
    assert len(LOGICAL_DATA) == 20
    assert hashlib.sha256(LOGICAL_DATA).digest() == LOGICAL_DIGEST

    provider_a = Provider(ordinal=1, generation=7, available=False)
    binding_a = Binding(provider_a, cap_generation=41)
    stale_a_token = binding_a.cap_generation
    a_root, a_file = 0x101, 0x102
    b_root, b_file = 0x201, 0x202
    assert len({a_root, a_file, b_root, b_file}) == 4

    assert run_provider(binding_a, stale_a_token, a_root, a_file, 1) is None
    assert provider_a.state == "fault-ready"
    provider_a.fault()
    binding_a.withdraw()
    assert provider_a.state == "faulted" and not binding_a.active
    try:
        binding_a.send(stale_a_token, tversion())
    except CapabilityDenied:
        pass
    else:
        raise AssertionError("withdrawn provider A capability remained usable")

    # Provider B does not become active until A is faulted, withdrawn, and its
    # old client capability has demonstrably become stale.
    provider_b = Provider(ordinal=2, generation=11, available=True)
    binding_b = Binding(provider_b, cap_generation=73)
    data = run_provider(
        binding_b, binding_b.cap_generation, b_root, b_file, 10
    )
    assert data == LOGICAL_DATA
    assert hashlib.sha256(data).digest() == LOGICAL_DIGEST
    assert provider_b.state == "clunked"

    # Mutants: wrong digest, write attempt, stale QID generation, nonzero
    # transport padding, and a cross-provider sequence leak all fail closed.
    wrong_digest = Provider(3, 1, True)
    assert expect(
        round_trip(Binding(wrong_digest, 1), 1, tattach(1, 0x301, bytes(32))),
        RERROR,
        1,
    ) == string(b"sequence")
    wrong_binding = Binding(wrong_digest, 2)
    expect(round_trip(wrong_binding, 2, tversion()), RVERSION, NOTAG)
    assert expect(
        round_trip(wrong_binding, 2, tattach(2, 0x301, bytes(32))),
        RERROR,
        2,
    ) == string(b"digest")

    no_write = Provider(4, 1, True)
    no_write_binding = Binding(no_write, 3)
    expect(round_trip(no_write_binding, 3, tversion()), RVERSION, NOTAG)
    expect(round_trip(no_write_binding, 3, tattach(2, 0x401)), RATTACH, 2)
    expect(round_trip(no_write_binding, 3, twalk(3, 0x401, 0x402)), RWALK, 3)
    expect(round_trip(no_write_binding, 3, topen(4, 0x402)), ROPEN, 4)
    before = LOGICAL_DATA
    assert expect(
        round_trip(no_write_binding, 3, twrite(5, 0x402, b"x")), RERROR, 5
    ) == string(b"sequence")
    assert LOGICAL_DATA == before and no_write.state == "opened"

    stale = bytearray(qid(0, provider_b.generation, provider_b.path(2)))
    stale[1:5] = p32(provider_b.generation + 1)
    assert bytes(stale) != qid(0, provider_b.generation, provider_b.path(2))

    padded = bytearray(tversion().ljust(MSIZE, b"\0"))
    padded[-1] = 1
    try:
        reassemble(tuple(
            bytes(padded[index : index + FRAGMENT_BYTES])
            for index in range(0, MSIZE, FRAGMENT_BYTES)
        ))
    except ValueError:
        pass
    else:
        raise AssertionError("nonzero transport padding was accepted")

    assert expect(
        round_trip(
            Binding(Provider(5, 1, True), 4), 4, tclunk(1, a_file)
        ),
        RERROR,
        1,
    ) == string(b"sequence")

    print(
        "M7B-1 native LogicalRead wire oracle: PASS "
        "(11 exact 9P requests, fault withdrawal + stale cap, "
        "2 independent providers, 5 mutant classes)"
    )


if __name__ == "__main__":
    main()
