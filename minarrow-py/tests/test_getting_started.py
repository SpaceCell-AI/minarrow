"""Tests for the getting-started surface, including the chunked containers and a
metadata-carrying schema attached to a ChunkedTable.

Run after `maturin develop`:
    python -m pytest tests/test_getting_started.py
"""

import minarrow as mp


# --- Arrays and tables ----------------------------------------------------


def test_array_basics():
    a = mp.Array([1, 2, 3, None])
    assert len(a) == 4
    assert a.dtype == mp.DType.Integer
    assert a.null_count == 1
    assert a[0] == 1
    assert a[3] is None


def test_table_basics():
    t = mp.Table({"id": [1, 2, 3], "px": [9.5, 10.0, 11.2]}, name="prices")
    assert t.name == "prices"
    assert t.n_rows == 3
    assert t.columns == ["id", "px"]


# --- Chunked containers ---------------------------------------------------


def test_chunked_array():
    ca = mp.ChunkedArray([mp.Array([1, 2, 3]), mp.Array([4, 5])], name="id")
    assert ca.n_chunks == 2
    assert len(ca) == 5
    assert ca.name == "id"
    assert ca.dtype == mp.DType.Integer
    assert [len(chunk) for chunk in ca.chunks] == [3, 2]
    assert len(ca.chunk(0)) == 3
    assert ca.chunk(99) is None


def test_chunked_table():
    ct = mp.ChunkedTable(
        [
            mp.Table({"id": [1, 2], "px": [9.5, 10.0]}),
            mp.Table({"id": [3], "px": [11.2]}),
        ],
        name="prices",
    )
    assert ct.n_batches == 2
    assert ct.n_chunks == 2
    assert ct.n_rows == 3
    assert ct.n_cols == 2
    assert ct.columns == ["id", "px"]
    assert ct.batch(0).columns == ["id", "px"]
    assert ct.batch(99) is None


def test_chunked_table_schema_with_metadata():
    schema = mp.Schema(
        [
            mp.Field("id", mp.ArrowType.Int64(), nullable=False, metadata={"role": "key"}),
            mp.Field("px", mp.ArrowType.Float64()),
        ],
        metadata={"source": "prices-feed", "version": "1"},
    )
    ct = mp.ChunkedTable(
        [
            mp.Table({"id": [1, 2], "px": [9.5, 10.0]}),
            mp.Table({"id": [3], "px": [11.2]}),
        ],
        name="prices",
        schema=schema,
    )
    assert ct.schema.names == ["id", "px"]
    assert ct.schema.metadata == {"source": "prices-feed", "version": "1"}
    assert ct.schema.fields[0].metadata == {"role": "key"}


def test_chunked_table_derives_schema_without_override():
    ct = mp.ChunkedTable([mp.Table({"a": [1, 2]})])
    assert ct.schema.names == ["a"]
    assert ct.schema.metadata == {}


# --- Arrow interop --------------------------------------------------------


def test_chunked_arrow_roundtrip():
    import pyarrow as pa

    ca = mp.ChunkedArray([mp.Array([1, 2, 3]), mp.Array([4, 5])])
    assert isinstance(ca.to_arrow(), pa.ChunkedArray)
    assert len(mp.ChunkedArray.from_arrow(ca.to_arrow())) == 5

    ct = mp.ChunkedTable([mp.Table({"id": [1, 2]}), mp.Table({"id": [3]})])
    assert ct.to_arrow().num_rows == 3
    assert mp.ChunkedTable.from_arrow(ct.to_arrow()).n_rows == 3
