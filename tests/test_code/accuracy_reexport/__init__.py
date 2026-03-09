"""Package __init__ that re-exports reexport_func from the impl submodule.

This creates the re-export chain:
    accuracy_reexport (package) -> accuracy_reexport.impl.reexport_func

The gap test checks that a caller doing `from accuracy_reexport import reexport_func`
and then calling `reexport_func()` does NOT currently get a uses edge to reexport_func
(shallow import/binding-propagation gap through __init__ re-exports).

Adapted from PyCG micro-benchmark imports/chained_import.
"""
from test_code.accuracy_reexport.impl import reexport_func  # noqa: F401  # re-exported

__all__ = ["reexport_func"]
