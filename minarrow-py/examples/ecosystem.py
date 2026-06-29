"""minarrow ecosystem interop

Shows the to_<runtime> / from_<runtime> bridges to the wider Arrow ecosystem.
Each runtime is skipped with a message when its package is not installed, so
this runs anywhere. Run with:
    python examples/ecosystem.py

"""

import importlib.util

import minarrow as ma


def section(title):
    print(f"\n{'=' * 4} {title} {'=' * 4}")


def have(module):
    return importlib.util.find_spec(module) is not None


arr = ma.Array([1, 2, 3, 4], name="value")
tbl = ma.Table({"id": [1, 2, 3], "px": [9.5, 10.0, 11.2], "sym": ["a", "b", "c"]}, name="prices")


section("Arrow PyCapsule interface")

# Array and Table implement __arrow_c_array__ / __arrow_c_stream__, so any
# Arrow-aware library reads minarrow without a large pyarrow dependency. The to_/from_
# methods are shims over that interface because each from_<runtime> is an alias 
# on from_arrow, which reads any producer's capsule.
print("Array and Table speak the Arrow PyCapsule interface.")
print("to_<runtime> / from_<runtime> are shims over it; from_<runtime>")
print("aliases from_arrow.")


# (label, module, has a single-array form)
RUNTIMES = [
    ("polars", "polars", True),
    ("duckdb", "duckdb", False),
    ("datafusion", "datafusion", False),
    ("daft", "daft", False),
    ("nanoarrow", "nanoarrow", True),
    ("pandas", "pandas", True),
    # ("cudf", "cudf", True),
    ("ibis", "ibis", False),
    ("narwhals", "narwhals", False),
]

for label, module, has_array in RUNTIMES:
    section(label)
    if not have(module):
        print(f"(skipped - `{module}` not installed)")
        continue
    to_name, from_name = f"to_{label}", f"from_{label}"
    try:
        if has_array and hasattr(arr, to_name):
            out = getattr(arr, to_name)()
            back = getattr(ma.Array, from_name)(out)
            print(f"Array  -> {type(out).__name__:18} -> from_{label} -> {type(back).__name__}")
        out_t = getattr(tbl, to_name)()
        back_t = getattr(ma.Table, from_name)(out_t)
        print(f"Table  -> {type(out_t).__name__:18} -> from_{label} -> {type(back_t).__name__}")
    except Exception as err:
        print(f"(error: {type(err).__name__}: {err})")


section("ADBC (databases)")

# ADBC needs a live connection, so it is not run here. Table.write_adbc(cursor,
# name) ingests into any ADBC database and Table.read_adbc(cursor) reads a
# result back, both over the Arrow PyCapsule interface.
print("Table.write_adbc(cursor, name)  - ingest into a database")
print("Table.read_adbc(cursor)         - read a result back")
print("Drivers: sqlite, postgresql, snowflake, bigquery, flightsql.")


print("\nDone.")
