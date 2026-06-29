# Quickstart

A runnable version of this guide is available at [`examples/quickstart.py`](https://github.com/pbower/minarrow/blob/main/minarrow-py/examples/quickstart.py).

## Arrays

Create an `Array` from any supported Python sequence:

```python
import minarrow as ma

array = ma.Array([1, 2, 3, None])

len(array)         # 4
array.dtype        # DType.Integer
array.dtype.group  # TypeClass.Numeric
array.bit_width    # 64
array.null_count   # 1

array[0]           # 1
array[3]           # None
array[1:3]         # Array containing [2, 3]
```

Minarrow infers the element type from the input. Python `None` values are represented as nulls.

### Named arrays

An array may carry a field name:

```python
ids = ma.Array([1, 2, 3], name="id")

ids.name  # "id"
```

### Nulls

Python `None` becomes a null. Nulls display as `null`, and the null mask is readable and editable in place:

```python
n = ma.Array([1, None, 3, None])

n             # [1, null, 3, null]
n.null_count  # 2
n.is_null()   # [False, True, False, True]
n[1]          # None

n.set(0, None)  # set a null
n.set(1, 99)    # clear a null by writing a value
```

### Categorical arrays

Intern strings into a dictionary with `dtype="categorical"` (aliases available are `"cat"`, `"cat8"`, `"cat16"`, `"cat32"`, `"cat64"` or `"category"`), or supply the dictionary yourself and pass the integer codes that index it. `None` is a null either way:

```python
auto = ma.Array(["red", "green", "red", "blue", None], dtype="categorical")
auto.dtype       # DType.Categorical
auto.arrow_type  # Dictionary(UInt32)

custom = ma.Array([0, 1, 2, None, 0], categories=["red", "green", "blue"])
custom           # ["red", "green", "blue", null, "red"]
```

## Tables

A `Table` is a named collection of equal-length arrays:

```python
table = ma.Table(
    {
        "id": [1, 2, 3],
        "price": [9.5, 10.0, 11.2],
    },
    name="prices",
)

table.name     # "prices"
table.n_rows   # 3
table.n_cols   # 2
table.columns  # ["id", "price"]
table.dtypes   # {"id": DType.Integer, "price": DType.Float}
table.schema   # Schema([id: Int64, price: Float64])
```

An unnamed table has `None` as its name.

A Minarrow `Table` corresponds to an Arrow record batch: every column has the same row count, and the table carries one schema.

## Indexing

Tables support positional row and column selection:

```python
table["price"]                  # Column by name -> Array
table[1:3]                      # Row slice -> Table
table[1:3, "price"]             # Row slice and one column -> Array
table[1:3, ["id", "price"]]     # Row slice and columns -> Table
table[:, 0]                     # Column by position -> Array
```

There is no row-label index.

* Integers select one position.
* Slices select a range.
* Column lists select multiple columns.
* Negative positions count from the end.
* Invalid positions raise an exception.

Slices return views where the underlying representation permits it.

## Fields and schemas

`ArrowType`, `Field` and `Schema` describe the table layout and its metadata.

```python
amount = ma.Field(
    "amount",
    ma.ArrowType.Float64(),
    nullable=False,
    metadata={"unit": "USD"},
)

amount.name        # "amount"
amount.arrow_type  # Float64
amount.nullable    # False
amount.metadata    # {"unit": "USD"}
```

Build a schema from fields:

```python
schema = ma.Schema(
    [
        ma.Field("id", ma.ArrowType.Int64(), nullable=False),
        amount,
    ]
)

schema.names      # ["id", "amount"]
schema["amount"]  # Field(name: amount, arrow_type: Float64, nullable: false)
```

See [Types and schemas](types.md) for the complete type system.

## Chunked arrays

A `ChunkedArray` represents one logical column as an ordered sequence of arrays:

```python
chunked = ma.ChunkedArray(
    [
        ma.Array([1, 2, 3]),
        ma.Array([4, 5]),
    ],
    name="id",
)

chunked.name      # "id"
chunked.n_chunks  # 2
len(chunked)      # 5
chunked.chunk(0)  # Array containing [1, 2, 3]
```

Chunked arrays avoid requiring all values to be combined into one contiguous allocation.

## Chunked tables

A `ChunkedTable` represents a logical table as an ordered sequence of table batches:

```python
chunked_table = ma.ChunkedTable(
    [
        ma.Table(
            {
                "id": [1, 2],
                "price": [9.5, 10.0],
            }
        ),
        ma.Table(
            {
                "id": [3],
                "price": [11.2],
            }
        ),
    ],
    name="prices",
)

chunked_table.name       # "prices"
chunked_table.n_batches  # 2
chunked_table.n_rows     # 3
chunked_table.batch(0)   # Table
```

Each batch must have a compatible schema.

### Schema metadata

A `ChunkedTable` can carry an explicit schema with table-level and field-level metadata:

```python
schema = ma.Schema(
    [
        ma.Field(
            "id",
            ma.ArrowType.Int64(),
            nullable=False,
            metadata={"role": "key"},
        ),
        ma.Field(
            "price",
            ma.ArrowType.Float64(),
        ),
    ],
    metadata={
        "source": "prices-feed",
        "version": "1",
    },
)

chunked_table = ma.ChunkedTable(
    [
        ma.Table(
            {
                "id": [1],
                "price": [9.5],
            }
        )
    ],
    name="prices",
    schema=schema,
)

chunked_table.schema.metadata
# {"source": "prices-feed", "version": "1"}

chunked_table.schema.fields[0].metadata
# {"role": "key"}
```

When a schema is supplied, `.schema` returns it rather than deriving a schema from the batches.

## Arrow interoperability

Minarrow imports and exports data through the Arrow PyCapsule interface.

```python
import minarrow as ma
import pyarrow as pa

arrow_array = pa.array([1, 2, 3])
arrow_batch = pa.RecordBatch.from_pydict(
    {
        "id": [1, 2, 3],
        "price": [9.5, 10.0, 11.2],
    }
)

array = ma.Array.from_arrow(arrow_array)
table = ma.Table.from_arrow(arrow_batch)
```

Export to PyArrow:

```python
pyarrow_array = array.to_arrow()
pyarrow_table = table.to_arrow()
```

Arrow-aware consumers can also use the capsule methods directly:

```python
pyarrow_array = pa.array(array)
pyarrow_table = pa.table(table)
```

The PyCapsule integration is zero-copy, so the Arrow buffers are shared without serialisation.

`ChunkedArray` and `ChunkedTable` also expose the Arrow PyCapsule interface:

```python
import polars as pl

frame = pl.from_arrow(chunked_table)
```

See [Ecosystem interoperability](interop.md) for Polars, DuckDB, pandas, DataFusion, cuDF and ADBC integrations.
