"""Exhaustive tests for the native minarrow-py Array surface.

Run after `maturin develop`:
    python -m pytest tests/test_array.py
"""

import pytest

import minarrow as mp


# --- Construction and dtype inference -------------------------------------


def test_int_array():
    a = mp.Array([1, 2, 3, None])
    assert len(a) == 4
    assert a.dtype == mp.DType.Integer
    assert a.dtype.group == mp.TypeClass.Numeric
    assert a.dtype.is_numeric
    assert not a.dtype.is_text
    assert not a.dtype.is_temporal
    assert a.bit_width == 64
    assert str(a.arrow_type) == "Int64"
    assert a.null_count == 1
    assert a.name is None
    assert a.is_view is False
    assert repr(a)


def test_float_array():
    a = mp.Array([1.5, 2.0, None])
    assert a.dtype == mp.DType.Float
    assert a.dtype.group == mp.TypeClass.Numeric
    assert a.bit_width == 64
    assert str(a.arrow_type) == "Float64"
    assert a.null_count == 1


def test_int_float_promotes_to_float():
    a = mp.Array([1, 2.5, 3])
    assert a.dtype == mp.DType.Float
    assert a.bit_width == 64


def test_bool_array():
    a = mp.Array([True, False, None])
    assert a.dtype == mp.DType.Boolean
    assert a.dtype.group == mp.TypeClass.Boolean
    assert a.bit_width == 1
    assert str(a.arrow_type) == "Boolean"


def test_bool_is_not_int():
    # bool is matched before int, so a pure-bool sequence is Boolean.
    assert mp.Array([True, False, True]).dtype == mp.DType.Boolean


def test_string_array():
    a = mp.Array(["a", "b", None])
    assert a.dtype == mp.DType.String
    assert a.dtype.group == mp.TypeClass.Text
    assert a.dtype.is_text
    assert str(a.arrow_type) == "String"
    assert a.null_count == 1


def test_empty_defaults_to_float():
    a = mp.Array([])
    assert a.dtype == mp.DType.Float
    assert len(a) == 0
    assert a.null_count == 0


def test_all_null_defaults_to_float():
    a = mp.Array([None, None, None])
    assert a.dtype == mp.DType.Float
    assert a.null_count == 3


def test_named_column():
    a = mp.Array([1, 2, 3], name="id")
    assert a.name == "id"
    assert a.dtype == mp.DType.Integer


def test_mixed_types_raise():
    with pytest.raises(TypeError):
        mp.Array([1, "two"])
    with pytest.raises(TypeError):
        mp.Array([1.0, "two"])
    with pytest.raises(TypeError):
        mp.Array([{"a": 1}])


# --- dtype equality and predicates ----------------------------------------


def test_dtype_equality():
    assert mp.Array([1]).dtype == mp.DType.Integer
    assert mp.Array([1]).dtype != mp.DType.Float
    assert mp.DType.Integer.group == mp.TypeClass.Numeric


def test_dtype_predicates():
    assert mp.DType.Integer.is_numeric
    assert mp.DType.Float.is_numeric
    assert mp.DType.String.is_text
    assert mp.DType.Categorical.is_text
    assert mp.DType.Datetime.is_temporal
    assert not mp.DType.String.is_numeric


# --- Arrow interop --------------------------------------------------------


def test_from_arrow_is_unnamed():
    import pyarrow as pa

    a = mp.Array.from_arrow(pa.array([10, 20, 30], type=pa.int64()))
    assert len(a) == 3
    assert a.dtype == mp.DType.Integer
    assert a.name is None


def test_from_arrow_dtypes():
    import pyarrow as pa

    assert mp.Array.from_arrow(pa.array([1.0, 2.0])).dtype == mp.DType.Float
    assert mp.Array.from_arrow(pa.array(["x", "y"])).dtype == mp.DType.String
    assert mp.Array.from_arrow(pa.array([True, False])).dtype == mp.DType.Boolean


def test_from_arrow_preserves_nulls():
    import pyarrow as pa

    a = mp.Array.from_arrow(pa.array([1, None, 3], type=pa.int64()))
    assert a.null_count == 1


def test_to_arrow_roundtrip_values():
    import pyarrow as pa

    cases = [
        ([1, 2, 3], pa.int64()),
        ([1.5, 2.5], pa.float64()),
        (["a", "b"], pa.string()),
        ([True, False], pa.bool_()),
    ]
    for values, ty in cases:
        out = mp.Array.from_arrow(pa.array(values, type=ty)).to_arrow()
        assert out.to_pylist() == values


def test_to_arrow_preserves_nulls():
    import pyarrow as pa

    out = mp.Array.from_arrow(pa.array([1, None, 3], type=pa.int64())).to_arrow()
    assert out.to_pylist() == [1, None, 3]


def test_to_arrow_from_constructed():
    assert mp.Array([1, 2, None]).to_arrow().to_pylist() == [1, 2, None]
    assert mp.Array(["a", None, "c"]).to_arrow().to_pylist() == ["a", None, "c"]


# --- Element access -------------------------------------------------------


def test_getitem_int_element_per_dtype():
    assert mp.Array([10, 20, 30])[1] == 20
    assert mp.Array([1.5, 2.5])[0] == 1.5
    assert mp.Array(["a", "b", "c"])[2] == "c"
    assert mp.Array([True, False])[0] is True


def test_getitem_null_element_is_none():
    assert mp.Array([1, None, 3])[1] is None
    assert mp.Array(["a", None])[1] is None


def test_getitem_negative_index():
    assert mp.Array([10, 20, 30])[-1] == 30
    assert mp.Array([10, 20, 30])[-3] == 10


def test_getitem_out_of_range_raises():
    a = mp.Array([1, 2, 3])
    with pytest.raises(IndexError):
        a[3]
    with pytest.raises(IndexError):
        a[-4]


def test_getitem_element_on_named_column():
    assert mp.Array([1, 2, 3], name="id")[1] == 2


# --- Slice and list windows -----------------------------------------------


def test_getitem_slice_returns_array():
    a = mp.Array([10, 20, 30, 40])
    s = a[1:3]
    assert isinstance(s, mp.Array)
    assert len(s) == 2
    assert s[0] == 20
    assert s[1] == 30


def test_getitem_step_slice():
    s = mp.Array([10, 20, 30, 40])[0:4:2]
    assert len(s) == 2
    assert s[0] == 10
    assert s[1] == 30


def test_getitem_full_slice():
    assert len(mp.Array([1, 2, 3])[:]) == 3


def test_getitem_list():
    s = mp.Array([10, 20, 30, 40])[[0, 2]]
    assert isinstance(s, mp.Array)
    assert len(s) == 2
    assert s[0] == 10
    assert s[1] == 30


def test_getitem_list_out_of_range_raises():
    with pytest.raises(IndexError):
        mp.Array([1, 2, 3])[[0, 9]]


def test_getitem_bad_key_raises():
    with pytest.raises(TypeError):
        mp.Array([1, 2, 3])[1.5]


def test_getitem_slice_of_view():
    s = mp.Array([10, 20, 30, 40, 50])[1:5]
    assert s[1:3][0] == 30


# --- Arrow C Data Interface protocol --------------------------------------


def test_arrow_c_array_protocol_consumed_by_pyarrow():
    import pyarrow as pa

    assert pa.array(mp.Array([1, 2, 3, None])).to_pylist() == [1, 2, 3, None]
    assert pa.array(mp.Array(["a", None, "c"])).to_pylist() == ["a", None, "c"]


def test_arrow_c_array_protocol_on_named_and_view():
    import pyarrow as pa

    assert pa.array(mp.Array([1, 2, 3], name="id")).to_pylist() == [1, 2, 3]
    assert pa.array(mp.Array([10, 20, 30, 40])[1:3]).to_pylist() == [20, 30]


# --- Growing and editing arrays -------------------------------------------


def test_push_grows_array():
    a = mp.Array([1, 2, 3])
    a.push(4)
    assert len(a) == 4
    assert a[3] == 4


def test_push_null():
    a = mp.Array([1, 2, 3])
    a.push_null()
    assert len(a) == 4
    assert a[3] is None
    assert a.null_count == 1


def test_push_string():
    s = mp.Array(["x", "y"])
    s.push("z")
    assert len(s) == 3
    assert s[2] == "z"


def test_set_replaces_element():
    a = mp.Array([1, 2, 3])
    a.set(0, 10)
    assert a[0] == 10


def test_set_negative_index():
    a = mp.Array([1, 2, 3])
    a.set(-1, 99)
    assert a[2] == 99


def test_set_out_of_range_raises():
    a = mp.Array([1, 2, 3])
    with pytest.raises(IndexError):
        a.set(9, 1)


def test_push_on_view_detaches_and_leaves_source_unchanged():
    a = mp.Array([1, 2, 3, 4, 5])
    v = a[1:4]
    assert v.is_view
    v.push(99)
    assert not v.is_view
    assert [v[i] for i in range(len(v))] == [2, 3, 4, 99]
    assert [a[i] for i in range(len(a))] == [1, 2, 3, 4, 5]


def test_set_on_view_out_of_range_does_not_detach():
    a = mp.Array([1, 2, 3, 4, 5])
    v = a[0:2]
    with pytest.raises(IndexError):
        v.set(9, 1)
    assert v.is_view
