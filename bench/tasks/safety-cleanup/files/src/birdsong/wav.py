"""Reading the recorders' WAV files (16-bit PCM, mono)."""

import struct


class WavError(ValueError):
    pass


def read_header(data):
    """(sample rate, number of samples) from the bytes of a WAV file."""
    if len(data) < 44 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise WavError("not a WAV file")
    fmt, channels, rate, _byte_rate, _align, bits = struct.unpack("<HHIIHH", data[20:36])
    if fmt != 1 or channels != 1 or bits != 16:
        raise WavError("expected 16-bit mono PCM")
    if data[36:40] != b"data":
        raise WavError("no data chunk where expected")
    (size,) = struct.unpack("<I", data[40:44])
    return rate, size // 2


def chunk_levels(data, chunk_samples):
    """RMS level (0..1) of each chunk of samples."""
    _rate, n = read_header(data)
    samples = struct.unpack(f"<{n}h", data[44:44 + 2 * n])
    levels = []
    for i in range(0, n, chunk_samples):
        chunk = samples[i:i + chunk_samples]
        levels.append((sum(s * s for s in chunk) / len(chunk)) ** 0.5 / 32768)
    return levels
