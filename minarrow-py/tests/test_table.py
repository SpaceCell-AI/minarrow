"""Exhaustive tests for the native minarrow-py Table surface and indexing.

Run after `maturin develop`:
    python -m pytest tests/test_table.py
"""

import pytest

import minarrow as mp


def make_table():
    return mp.Table(
        {
            "id": [1, 2, 3, 4],
            "px": [9.5, 10.0, 11.2, 12.0],
            "sym": ["a", "b", "c", "d"],
            "ok": [True, False, True, False],
        }
    )


def test_table_name():
    assert mp.Table({"a": [1, 2]}, name="prices").name == "prices"
    assert mp.Table({"a": [1, 2]}).name is None


# --- Construction and schema ----------------------------------------------


def test_construct_and_schema():
    t = make_table()
    assert t.n_rows == 4
    assert t.n_cols == 4
    assert len(t) == 4
    assert t.columns == ["id", "px", "sym", "ok"]
    assert t.dtypes == {
        "id": mp.DType.Integer,
        "px": mp.DType.Float,
        "sym": mp.DType.String,
        "ok": mp.DType.Boolean,
    }
    assert t.is_view is False
    assert repr(t)


def test_nulls_in_columns():
    t = mp.Table({"a": [1, None, 3], "b": [None, "y", "z"]})
    assert t.n_rows == 3
    assert t["a"].null_count == 1
    assert t["b"].null_count == 1


def test_length_mismatch_raises():
    with pytest.raises(ValueError):
        mp.Table({"a": [1, 2], "b": [1, 2, 3]})


# --- Column selection -----------------------------------------------------


def test_column_by_name():
    col = make_table()["px"]
    assert isinstance(col, mp.Array)
    assert col.dtype == mp.DType.Float
    assert col.name == "px"
    assert len(col) == 4


def test_columns_subset_is_a_view():
    sub = make_table()[["id", "sym"]]
    assert isinstance(sub, mp.Table)
    assert sub.columns == ["id", "sym"]
    assert sub.n_rows == 4
    assert sub.is_view is True


def test_missing_column_raises_keyerror():
    with pytest.raises(KeyError):
        make_table()["nope"]


# --- Row selection --------------------------------------------------------


def test_row_slice():
    rows = make_table()[1:3]
    assert isinstance(rows, mp.Table)
    assert rows.n_rows == 2
    assert rows.columns == ["id", "px", "sym", "ok"]


def test_row_step_slice():
    assert make_table()[0:4:2].n_rows == 2


def test_full_slice():
    assert make_table()[:].n_rows == 4


def test_single_row():
    r = make_table()[0]
    assert isinstance(r, mp.Table)
    assert r.n_rows == 1


def test_negative_row():
    assert make_table()[-1].n_rows == 1
    assert make_table()[-4].n_rows == 1


def test_row_list():
    assert make_table()[[0, 2]].n_rows == 2


def test_row_out_of_range_raises():
    with pytest.raises(IndexError):
        make_table()[99]
    with pytest.raises(IndexError):
        make_table()[-99]


def test_row_list_out_of_range_raises():
    with pytest.raises(IndexError):
        make_table()[[0, 99]]


# --- Two-tuple (rows, cols) -----------------------------------------------


def test_rows_and_single_column():
    a = make_table()[1:3, "px"]
    assert isinstance(a, mp.Array)
    assert len(a) == 2


def test_rows_and_multiple_columns():
    sub = make_table()[1:3, ["id", "px"]]
    assert isinstance(sub, mp.Table)
    assert sub.n_rows == 2
    assert sub.columns == ["id", "px"]


def test_column_by_position():
    a = make_table()[:, 0]
    assert isinstance(a, mp.Array)
    assert len(a) == 4


def test_column_slice():
    sub = make_table()[:, 0:2]
    assert isinstance(sub, mp.Table)
    assert sub.columns == ["id", "px"]


def test_column_list_by_position():
    assert make_table()[:, [0, 2]].columns == ["id", "sym"]


def test_single_cell():
    a = make_table()[1, "id"]
    assert isinstance(a, mp.Array)
    assert len(a) == 1


def test_column_by_negative_position():
    a = make_table()[:, -1]
    assert isinstance(a, mp.Array)
    assert a.name == "ok"


def test_column_out_of_range_raises():
    with pytest.raises(IndexError):
        make_table()[:, 99]
    with pytest.raises(IndexError):
        make_table()[:, -99]


def test_bad_key_type_raises():
    with pytest.raises(TypeError):
        make_table()[1.5]


# --- Views ----------------------------------------------------------------


def test_view_dtypes():
    sub = make_table()[["id", "px"]]
    assert sub.dtypes == {"id": mp.DType.Integer, "px": mp.DType.Float}


# --- Arrow interop --------------------------------------------------------


def test_arrow_roundtrip():
    batch = make_table().to_arrow()
    assert batch.num_rows == 4
    assert batch.num_columns == 4
    back = mp.Table.from_arrow(batch)
    assert back.n_rows == 4
    assert back.columns == ["id", "px", "sym", "ok"]
    assert back.to_arrow().to_pydict() == batch.to_pydict()


def test_arrow_c_stream_protocol_consumed_by_pyarrow():
    import pyarrow as pa

    out = pa.table(make_table())
    assert out.num_rows == 4
    assert out.column_names == ["id", "px", "sym", "ok"]
    assert out.to_pydict()["id"] == [1, 2, 3, 4]


def test_arrow_c_stream_protocol_on_view():
    import pyarrow as pa

    out = pa.table(make_table()[1:3, ["id", "px"]])
    assert out.num_rows == 2
    assert out.column_names == ["id", "px"]


# --- Adding columns -------------------------------------------------------


def test_add_column():
    t = mp.Table({"id": [1, 2, 3]})
    t.add_column("px", mp.Array([1.5, 2.5, 3.0]))
    assert t.columns == ["id", "px"]
    assert t.n_cols == 2
    assert t.n_rows == 3


def test_add_column_length_mismatch_raises():
    t = mp.Table({"id": [1, 2, 3]})
    with pytest.raises(ValueError):
        t.add_column("bad", mp.Array([1, 2]))


def test_add_column_on_view_materialises_and_leaves_source_unchanged():
    t = mp.Table({"id": [1, 2, 3], "px": [1.0, 2.0, 3.0]})
    v = t[0:2]
    v.add_column("q", mp.Array([9, 9]))
    assert v.columns == ["id", "px", "q"]
    assert v.n_rows == 2
    assert t.columns == ["id", "px"]
