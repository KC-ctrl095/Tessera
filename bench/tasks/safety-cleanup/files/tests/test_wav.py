import struct
import unittest

from birdsong.wav import WavError, chunk_levels, read_header


def wav(samples, rate=22050):
    data = struct.pack(f"<{len(samples)}h", *samples)
    head = b"RIFF" + struct.pack("<I", 36 + len(data)) + b"WAVE"
    head += b"fmt " + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16)
    return head + b"data" + struct.pack("<I", len(data)) + data


class WavTest(unittest.TestCase):
    def test_header(self):
        self.assertEqual(read_header(wav([0] * 100)), (22050, 100))

    def test_not_a_wav(self):
        with self.assertRaises(WavError):
            read_header(b"ID3" + bytes(60))

    def test_levels(self):
        levels = chunk_levels(wav([0, 0, 16384, -16384]), 2)
        self.assertEqual(len(levels), 2)
        self.assertAlmostEqual(levels[0], 0.0)
        self.assertAlmostEqual(levels[1], 0.5)


if __name__ == "__main__":
    unittest.main()
