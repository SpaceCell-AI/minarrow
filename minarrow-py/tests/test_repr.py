"""Repr surface: a consistent single summary plus a values preview.

Run after `maturin develop`:
    python -m pytest tests/test_repr.py
"""

import minarrow as mp


def test_array_repr_is_consistent():
    text = repr(mp.Array([1, 2, 3]))
    assert text.startswith("Array(")
    assert "dtype: Integer" in text
    assert "bit_width: 64" in text
    assert "dtype_group: Numeric" in text
    assert "len: 3" in text
    assert "nulls: 0" in text
    assert "[1, 2, 3]" in text


def test_array_repr_reports_name_and_nulls_once():
    text = repr(mp.Array([1.5, None], name="px"))
    assert "name: px" in text
    assert text.count("nulls:") == 1
    assert "nulls: 1" in text
    assert "null" in text


def test_array_repr_caps_long_preview():
    text = repr(mp.Array(list(range(50))))
    assert "..." in text


def test_dtype_and_group_repr_are_plain_names():
    dtype = mp.Array([1]).dtype
    assert repr(dtype) == "Integer"
    assert str(dtype) == "Integer"
    assert repr(dtype.group) == "Numeric"
