"""User module for __all__-aware star import tests.

Imports everything from star_all_src via ``from … import *`` and calls all four
functions.  Only the two names listed in ``__all__`` should resolve; the other
two must remain unresolved (runtime NameErrors).
"""
from test_code.star_all_src import *  # noqa: F401,F403


def all_aware_caller():
    """Calls all four names from the source module."""
    public_exported()         # in __all__ → must resolve  # noqa: F821
    _special_exported()       # in __all__ (private) → must resolve  # noqa: F821
    public_not_exported()     # NOT in __all__ → must NOT resolve concretely  # noqa: F821
    _private_not_exported()   # NOT in __all__ → must NOT resolve concretely  # noqa: F821
