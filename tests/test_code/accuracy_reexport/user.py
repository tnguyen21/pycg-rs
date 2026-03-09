"""User of the re-exported function (for the accuracy reexport gap test).

Imports reexport_func from the accuracy_reexport package (which re-exports
it from accuracy_reexport.impl via __init__.py) and calls it in a function.

Known gap: the import-chain binding via __init__.py re-export is not currently
propagated, so `caller` does NOT get a uses edge to reexport_func.

Adapted from PyCG micro-benchmark imports/chained_import.
"""
from test_code.accuracy_reexport import reexport_func  # noqa: F401  # test fixture


def reexport_caller():
    """Calls reexport_func imported through the package re-export chain."""
    reexport_func()
