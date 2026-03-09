"""Implementation module for the re-export accuracy fixture.

Defines reexport_func, which is imported and re-exported by the package
__init__.py.  A user doing `from accuracy_reexport import reexport_func`
should (ideally) get a uses edge to this function; currently that chain is
not propagated (the known binding-propagation gap through __init__ re-exports).
"""


def reexport_func():
    """This function is re-exported from the accuracy_reexport package."""
    pass
