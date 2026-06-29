"""minarrow quickstart guide

Run it:
    python examples/quickstart.py

"""

import minarrow as ma


def section(title):
    print(f"\n{'=' * 4} {title} {'=' * 4}")


# --- Arrays ----------------------------------------------------------------

section("Construct arrays")

ints = ma.Array([1, 2, 3, 4])
floats = ma.Array([1.5, 2.5, 3.0])
strings = ma.Array(["alice", "bob", "carol"])
flags = ma.Array([True, False, True])
with_nulls = ma.Array([1, 2, None, 4])

print("ints   :", repr(ints))
print("nulls  :", with_nulls, "| null_count:", with_nulls.null_count)

section("Dtype surface")

print("ints.dtype        :", ints.dtype)              # DType.Integer
print("ints.dtype.group  :", ints.dtype.group)        # TypeClass.Numeric
print("ints.bit_width    :", ints.bit_width)          # 64
print("ints.arrow_type   :", ints.arrow_type)         # Int64
print("strings.dtype     :", strings.dtype)           # DType.String
print("strings.is_text   :", strings.dtype.is_text)   # True
print("floats.is_numeric :", floats.dtype.is_numeric) # True

section("Named columns")

ids = ma.Array([10, 20, 30], name="id")
print("ids.name:", ids.name)

section("Index arrays")

a = ma.Array([10, 20, 30, 40, 50])
print("a[0]      :", a[0])           # 10
print("a[-1]     :", a[-1])          # 50
print("a[1:3]    :", a[1:3])         # window [20, 30]
print("a[[0, 2]] :", a[[0, 2]])      # gather [10, 30]
print("null element :", with_nulls[2])  # None
try:
    a[99]
except IndexError as err:
    print("a[99]     :", f"IndexError ({err})")


# --- Nulls -----------------------------------------------------------------

section("Null Masks")

n = ma.Array([1, None, 3, None])
print("array      :", n)            # [1, null, 3, null]
print("null_count :", n.null_count)  # 2
print("is_null    :", n.is_null())  # [False, True, False, True]
print("n[1]       :", n[1])         # None

# Edit the null mask in place
n.set(0, None)
n.set(1, 99)
print("after edits:", n, "| null_count:", n.null_count)


# --- Categorical -----------------------------------------------------------

section("Categorical arrays")

# Add string categories into the dictionary with dtype="categorical".
# Other specific options include "cat", "cat32", "category".
auto = ma.Array(["red", "green", "red", "blue", None], dtype="categorical")
print("from values:", auto, "|", auto.dtype, "|", auto.arrow_type)

# Alternatively, one can supply the dictionary and pass the integer codes that index it.
custom = ma.Array([0, 1, 2, None, 0], categories=["red", "green", "blue"])
print("custom dict:", custom, "| is_null:", custom.is_null())


# --- Tables ----------------------------------------------------------------

section("Construct a table")

t = ma.Table(
    {
        "id": [1, 2, 3, 4],
        "px": [9.5, 10.0, 11.2, 12.0],
        "sym": ["a", "b", "c", "d"],
    },
    name="prices",
)
print(t)
print("name:", t.name, "| n_rows:", t.n_rows, "| n_cols:", t.n_cols)
print("columns:", t.columns)
print("dtypes :", t.dtypes)

section("Pandas-style indexing")

print("t['px']            ->", type(t["px"]).__name__)
print("t[1:3]             ->", type(t[1:3]).__name__, f"({t[1:3].n_rows} rows)")
print("t[1:3, 'px']       ->", type(t[1:3, "px"]).__name__)
print("t[1:3, ['id','px']]->", type(t[1:3, ["id", "px"]]).__name__, t[1:3, ["id", "px"]].columns)
print("t[:, 0]            ->", type(t[:, 0]).__name__, "name:", t[:, 0].name)


# --- Mutate / build incrementally ------------------------------------------

section("Grow and edit arrays")

print(
    "Note: Arrow arrays are immutable once constructed, which is what makes them "
    "safe to share. Minarrow still lets you edit them. When the underlying memory "
    "is shared, for example another object or process is reading the same Array, "
    "the edit copies into a new owned array first, so the shared data isn't "
    "changed in place."
)

col = ma.Array([1, 2, 3])
col.push(4)          # append a value
col.set(0, 10)       # overwrite by index, negatives count from the end
col.set(-1, 40)
col.push_null()      # append a null
print("after push/set :", col, "| null_count:", col.null_count)

# Pushing onto a view materialises it to an owned array. The source is unchanged.
base = ma.Array([1, 2, 3, 4, 5])
window = base[1:4]
window.push(99)
print("view grew to    :", window, "| is_view:", window.is_view)
print("source unchanged:", base)

section("Add a column to a table")

prices = ma.Table({"id": [1, 2, 3]}, name="prices")
prices.add_column("px", ma.Array([1.5, 2.5, 3.0]))
print("columns:", prices.columns, "| n_cols:", prices.n_cols)


# --- Types, fields & schemas -----------------------------------------------

section("Arrow types")

# ArrowType mirrors minarrow's logical type. pyo3 makes each variant callable,
# so a non-parametric type is built with a call. Parametric types take their
# unit and parameters as arguments.
print("Int64      :", ma.ArrowType.Int64())
print("Float64    :", ma.ArrowType.Float64())
print("String     :", ma.ArrowType.String())
print("Timestamp  :", ma.ArrowType.Timestamp(ma.TimeUnit.Microseconds, "UTC"))
print("Time32     :", ma.ArrowType.Time32(ma.TimeUnit.Milliseconds))
print("Dictionary :", ma.ArrowType.Dictionary(index=ma.CategoricalIndexType.UInt32))

section("Fields")

# A Field names a column and carries its type, nullability, and optional metadata.
amount = ma.Field("amount", ma.ArrowType.Float64(), nullable=False, metadata={"unit": "USD"})
print("field      :", amount)
print("name       :", amount.name)        # amount
print("arrow_type :", amount.arrow_type)  # Float64
print("dtype      :", amount.dtype)        # DType.Float
print("nullable   :", amount.nullable)    # False
print("metadata   :", amount.metadata)    # {'unit': 'USD'}

section("Schemas")

# A Schema is an ordered set of Fields plus optional schema-level metadata.
schema = ma.Schema(
    [
        ma.Field("id", ma.ArrowType.Int64(), nullable=False),
        amount,
        ma.Field("sym", ma.ArrowType.String()),
    ],
    metadata={"dataset": "prices"},
)
print(schema)
print("names      :", schema.names)       # ['id', 'amount', 'sym']
print("len        :", len(schema))        # 3
print("metadata   :", schema.metadata)    # {'dataset': 'prices'}
print("by name    :", schema["amount"])   # Field(name: amount, ...)
print("by index   :", schema[0])          # Field(name: id, ...)
try:
    schema["missing"]
except KeyError as err:
    print("missing    :", f"KeyError ({err})")

# Every Table exposes its Schema.
print("t.schema   :", t.schema)


# --- Arrow interop ---------------------------------------------------------

section("Arrow interop (PyArrow)")

import pyarrow as pa

# In - from a PyArrow array / record batch
a_in = ma.Array.from_arrow(pa.array([7, 8, 9]))
t_in = ma.Table.from_arrow(pa.RecordBatch.from_pydict({"x": [1, 2], "y": [1.0, 2.0]}))
print("from_arrow array :", a_in)
print("from_arrow table :", t_in.columns, f"({t_in.n_rows} rows)")

# Out - explicit, or via the Arrow PyCapsule protocol
print("a.to_arrow()      :", ints.to_arrow().to_pylist())
print("pa.array(a)       :", pa.array(ints).to_pylist())     # __arrow_c_array__
print("pa.table(t)       :", pa.table(t).column_names)       # __arrow_c_stream__

# Round-trip
back = ma.Array.from_arrow(pa.array(strings)).to_arrow().to_pylist()
print("round-trip        :", back)


# --- Polars interop --------------------------------------------------------

section("Polars interop")

try:
    import polars as pl
except ImportError:
    print("(polars not installed - run `pip install polars` to see this)")
else:
    # Array -> Series, Table -> DataFrame, over the Arrow C Data Interface.
    # A named array carries its name onto the Series.
    print("polars version   :", pl.__version__)
    print("Array.to_polars  :", ints.to_polars().to_list())
    print("named -> Series  :", repr(ids.to_polars().name))  # 'id'
    print("Table.to_polars  ->", type(t.to_polars()).__name__)
    print(t.to_polars())

print("\nDone.")
