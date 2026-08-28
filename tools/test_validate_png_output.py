#!/usr/bin/env python3
"""Regression tests for the independent PNG scanline decoder."""

from __future__ import annotations

import unittest

import validate_png_output


def reference_paeth(left: int, up: int, upper_left: int) -> tuple[int, str]:
    estimate = left + up - upper_left
    distances = (
        abs(estimate - left),
        abs(estimate - up),
        abs(estimate - upper_left),
    )
    if distances[0] <= distances[1] and distances[0] <= distances[2]:
        return left, "left"
    if distances[1] <= distances[2]:
        return up, "up"
    return upper_left, "upper-left"


def reference_unfilter(raw: bytes, width: int, height: int) -> tuple[bytes, set[str]]:
    stride = width * 4
    at = 0
    previous = bytearray()
    decoded = bytearray()
    predictors: set[str] = set()
    for _ in range(height):
        kind = raw[at]
        at += 1
        assert kind == 4
        line = bytearray(raw[at : at + stride])
        at += stride
        for index in range(stride):
            left = line[index - 4] if index >= 4 else 0
            up = previous[index] if previous else 0
            upper_left = previous[index - 4] if previous and index >= 4 else 0
            predictor, selected = reference_paeth(left, up, upper_left)
            predictors.add(selected)
            line[index] = (line[index] + predictor) & 0xFF
        decoded += line
        previous = line
    return bytes(decoded), predictors


class PngUnfilterTests(unittest.TestCase):
    def test_paeth_matches_reference_across_all_predictors(self) -> None:
        width = 2
        height = 8
        raw = b"".join(
            bytes([4])
            + bytes((row * 37 + column * 53) & 0xFF for column in range(width * 4))
            for row in range(height)
        )
        expected, predictors = reference_unfilter(raw, width, height)
        self.assertEqual(predictors, {"left", "up", "upper-left"})
        self.assertEqual(validate_png_output.unfilter(raw, width, height), expected)


if __name__ == "__main__":
    unittest.main()
