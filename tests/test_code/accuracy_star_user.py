"""User module for the from-star-import accuracy fixture.

Imports everything from accuracy_star_src via `from … import *` and calls
both exported functions.  The caller's uses edges must resolve to the two
functions even though they were not explicitly named in the import statement.

Adapted from PyCG micro-benchmark imports/import_all.
"""
from test_code.accuracy_star_src import *  # noqa: F401,F403  # test fixture


def star_import_caller():
    """Both calls must produce uses edges despite the wildcard import."""
    exported_func1()  # noqa: F821  # defined via star import
    exported_func2()  # noqa: F821
