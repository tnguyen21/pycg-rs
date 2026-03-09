"""Source module without ``__all__`` for privacy-filter star-import tests.

Public names should be importable via ``from star_private_src import *``.
Private names (``_``-prefixed) must be excluded by the default privacy filter.
"""


def public_func():
    """Public — should be injectable by star import."""
    pass


def _private_impl():
    """Private — must be excluded by star import (no __all__)."""
    pass
